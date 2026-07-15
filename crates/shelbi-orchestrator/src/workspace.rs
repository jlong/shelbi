//! Workspace lifecycle: the pre-declared agent slots that pick up Kanban
//! tasks. See [`crate::ensure_dashboard`] for the project's overall tmux
//! layout; this module is concerned only with the per-workspace slot.
//!
//! Each workspace owns a stable worktree at
//! `<machine.work_dir>/.shelbi/wt/<workspace-name>`. The worktree persists
//! across tasks; the workspace switches branches between assignments. The
//! workspace's tmux pane (window for local hub workspaces, session for remote
//! workspaces) is killed and re-created on every assignment to clear the
//! agent's context — that's the user-specified semantics.
//!
//! Reviewer hint: this module does no state writes to task files; the
//! caller (CLI) is responsible for updating `assigned_to` / `branch` /
//! `column`. We just stand up the worktree + tmux pane + claude.

use std::path::{Path, PathBuf};

use shelbi_core::{
    validate_task_id, Error, Host, Machine, Project, PromptInjectionKind, Result, TmuxAddr,
    WorkspaceSpec,
};

/// Absolute path to the currently-running `shelbi` binary, so the
/// wrapper invocation we hand to tmux is anchored to *this* build
/// rather than whatever happens to be on PATH inside the pane's
/// shell. Mirrors the helper in `crate::current_exe_string`; kept
/// module-local so workspace.rs has no upward dependency on lib.rs.
fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned())
}

/// Filter `git status --porcelain` output down to the lines that represent
/// *user-authored* changes — i.e. drop shelbi's own footprint in the worktree.
///
/// Two families of paths are shelbi's, not the user's, and must never block a
/// dispatch or rebase:
///
/// - `.claude/` — shelbi's deploy footprint (`settings.json`, the
///   `shelbi-ready` marker). Normally gitignored, but a repo that
///   hasn't ignored it yet still shouldn't wedge on it.
/// - `.shelbi/` — shelbi's own runtime scratch (`.shelbi/messages/` inter-agent
///   mail and other daemon state) written into the worktree root. It is
///   never user work, so an otherwise-idle worktree whose only untracked entry
///   is `.shelbi/` must read as clean and dispatchable.
///
/// Callers pass `git status --porcelain -z` (NUL-delimited) output. The `-z`
/// form is load-bearing: plain porcelain quotes paths with unusual bytes
/// (`"a\tb"`) and renders renames as `orig -> new`, both of which defeat a
/// naive `line.get(3..)` carve-out and can mis-classify one of shelbi's own
/// paths as user work (a spurious "uncommitted changes" block). Under `-z`
/// paths are emitted verbatim and each record is `XY <path>`; a rename/copy
/// (`R`/`C`) carries its origin path as the *next* NUL-separated field, which
/// we consume so it isn't re-parsed as a status record. The carve-out is
/// keyed on the destination path.
///
/// Returns the surviving records (each still `XY <path>`) so error messages
/// keep their familiar shape.
fn user_dirty_porcelain_lines(porcelain_z: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut records = porcelain_z.split('\0');
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        // Index or worktree status of R/C means an origin-path field trails
        // this record — pull it off the stream so the loop doesn't treat it
        // as its own entry.
        let bytes = record.as_bytes();
        let is_rename_or_copy =
            matches!(bytes.first(), Some(b'R' | b'C')) || matches!(bytes.get(1), Some(b'R' | b'C'));
        if is_rename_or_copy {
            let _ = records.next();
        }
        let path = record.get(3..).unwrap_or("");
        if !is_shelbi_scratch_path(path) {
            out.push(record);
        }
    }
    out
}

/// True when `path` (as it appears in porcelain output) is one of shelbi's
/// own worktree paths — `.claude/` deploy footprint or `.shelbi/` runtime
/// scratch — rather than user work. See [`user_dirty_porcelain_lines`].
fn is_shelbi_scratch_path(path: &str) -> bool {
    path.starts_with(".claude/")
        || path == ".claude"
        || path.starts_with(".shelbi/")
        || path == ".shelbi"
}

/// Where a workspace's pane lives in tmux. Local workspaces get a window in the
/// project session; remote workspaces get their own session (so they survive
/// SSH drops).
pub fn workspace_tmux_addr(project: &Project, workspace: &WorkspaceSpec) -> Result<TmuxAddr> {
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(workspace.machine.clone()))?;
    Ok(match machine.host() {
        Host::Local => TmuxAddr {
            session: format!("shelbi-{}", project.name),
            window: workspace.name.clone(),
        },
        Host::Ssh { .. } => TmuxAddr {
            session: format!("shelbi-w-{}", workspace.name),
            window: "agent".into(),
        },
    })
}

/// `<machine.work_dir>/.shelbi/wt/<workspace-name>` — the workspace's persistent
/// worktree path on its machine.
pub fn workspace_worktree(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    machine
        .work_dir
        .join(".shelbi")
        .join("wt")
        .join(&workspace.name)
}

/// The ready-handoff file marker for a workspace:
/// `<worktree>/.claude/shelbi-ready`.
///
/// The workspace writes its current task id here to signal "I'm done, advance
/// me"; the hub poller reads it (`stat`/`cat`, local or over SSH), moves the
/// task forward to its workflow's handoff status, and clears the file. This
/// replaces the old pane-title / `shelbi task move` handoff, both of which
/// raced Claude's own OSC title writes and the Stop hook. A file survives
/// both: nothing the agent's UI does can clobber it.
///
/// It lives under `.claude/` (not the worktree root) on purpose — `.claude/`
/// is where shelbi already deploys `settings.json`, and shelbi relies on it
/// being gitignored so deployed files don't dirty the worktree between
/// tasks. Keeping the marker there means it never shows up in
/// `git status --porcelain` and so never trips [`sync_worktree`]'s
/// clean-worktree check.
pub fn workspace_ready_marker(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    workspace_worktree(machine, workspace)
        .join(".claude")
        .join("shelbi-ready")
}

/// Read the ready-handoff marker, returning the task id the workspace wrote
/// into it (trimmed) or `None` if the marker is absent or empty. Works for
/// both local and remote workspaces — `cat` is routed through `shelbi-ssh`,
/// which is a no-op wrapper for [`Host::Local`].
///
/// The marker body is a plain task id, so we validate it against
/// [`validate_task_id`] — the same allowlist task ids pass everywhere else —
/// before returning it. That rejects two failure modes as an `Err` (which the
/// poller logs and, crucially, does *not* clear the marker on, so the signal
/// survives to the next tick): a torn write (a half-flushed id from a
/// non-atomic writer, though workers now write atomically) and a hostile body
/// (e.g. anything but a bare id that a stray program dropped into the file).
/// An invalid body never drives a board move.
pub fn read_ready_marker(host: &Host, marker: &Path) -> Result<Option<String>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error for us: the
        // workspace simply hasn't signalled it's done yet.
        return Ok(None);
    }
    let content = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if content.is_empty() {
        return Ok(None);
    }
    validate_task_id(&content).map_err(|_| {
        Error::Other(format!(
            "ready marker at {path} holds an invalid task id ({content:?}); leaving it in place"
        ))
    })?;
    Ok(Some(content))
}

/// Remove the ready-handoff marker (idempotent — `rm -f` succeeds if absent).
/// Called once the poller has consumed the signal, and again at task start to
/// clear any stale marker before the worktree is reused.
pub fn clear_ready_marker(host: &Host, marker: &Path) -> Result<()> {
    let path = marker.to_string_lossy().into_owned();
    shelbi_ssh::run(host, ["rm", "-f", path.as_str()]).map_err(Error::Io)?;
    Ok(())
}

/// The agent-written **transition** marker for a workspace:
/// `<worktree>/.claude/shelbi-transition`.
///
/// Where [`workspace_ready_marker`] is the forward-only "I'm done, move me to
/// review" handoff, this marker lets an agent request an *arbitrary* status
/// transition for its own task — including a **backward** bounce. A reviewer /
/// gate agent (e.g. an Adversarial Review or Security Review agent) that finds
/// problems writes this marker to send the task back to an active status; the
/// hub poller validates the requested edge against the task's workflow
/// `transitions` and, if allowed, applies the move (see
/// `shelbi_tui::poller::maybe_apply_transition`).
///
/// ## Format
///
/// Two lines of plain UTF-8 text:
///
/// ```text
/// <task-id>
/// <target-status>
/// ```
///
/// - **Line 1** is the task id the agent is working — it must be the
///   workspace's own currently-assigned task or the poller treats the marker
///   as stale/foreign and clears it without moving anything.
/// - **Line 2** is either a **target status id** (one of the workflow's
///   declared `statuses[].id`, e.g. `in-progress`) — the primitive — or a
///   short **verb** as sugar: `reject` / `bounce`, which the poller resolves
///   to the workflow's designated active (`active`-category) status. Trailing
///   blank lines / surrounding whitespace are ignored.
///
/// Any agent can write this file directly — workspaces have no `shelbi` binary,
/// so like the ready marker it is a plain file an agent's
/// `instructions.md` can teach it to write. To be torn-write safe, write to a
/// sibling temp path and atomically `mv` it into place, exactly as the
/// ready marker prompt does:
///
/// ```sh
/// printf '%s\n%s\n' <task-id> <target-status> \
///   > .claude/shelbi-transition.tmp && mv .claude/shelbi-transition.tmp .claude/shelbi-transition
/// ```
///
/// It lives under `.claude/` for the same reason as the other markers: that
/// directory is shelbi's gitignored deploy footprint, so the marker never
/// dirties the worktree between tasks and never trips a clean-worktree check.
pub fn workspace_transition_marker(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    workspace_worktree(machine, workspace)
        .join(".claude")
        .join("shelbi-transition")
}

/// A parsed transition request from the [`workspace_transition_marker`]: the
/// task id the agent is acting on and the raw target it wrote (a status id or a
/// verb like `reject` — resolution against the workflow happens poll-side, in
/// `decide_transition`, since it needs the loaded workflow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRequest {
    /// Task id from line 1 — validated against [`validate_task_id`].
    pub task_id: String,
    /// Raw target token from line 2 — a status id or a verb (`reject`/`bounce`).
    /// Validated only for shape here (a conservative id charset); its meaning
    /// is resolved against the workflow by the poller.
    pub target: String,
}

/// Read the transition marker, returning the [`TransitionRequest`] the workspace
/// wrote or `None` if the marker is absent or empty. Mirrors
/// [`read_ready_marker`]: routes `cat` through `shelbi-ssh` (a no-op for
/// [`Host::Local`]) and validates the body before returning it.
///
/// Two validations gate the return, and either failing surfaces as an `Err`
/// (which the poller logs and does NOT clear on — the signal survives to the
/// next tick, matching the ready marker's torn-write handling) rather than
/// driving a board move off garbage:
///
/// - **Line 1** (task id) must pass [`validate_task_id`] — the same allowlist
///   every task id passes everywhere else. Rejects a torn write (a half-flushed
///   id) and a hostile body.
/// - **Line 2** (target) must be a non-empty conservative id token
///   (alphanumeric plus `-` / `_`). The target is only ever compared against
///   workflow status ids and used to look up a column — never shell-interpolated
///   — but validating its shape keeps a stray program's junk out of the log and
///   the decision path.
///
/// Only the first two non-empty lines are consulted, so a multi-line body can't
/// tear across downstream log records.
pub fn read_transition_marker(host: &Host, marker: &Path) -> Result<Option<TransitionRequest>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error: the workspace simply
        // hasn't requested a transition.
        return Ok(None);
    }
    let content = String::from_utf8_lossy(&out.stdout);
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(task_id) = lines.next() else {
        // Present but empty/whitespace — treat as no request (poller clears it).
        return Ok(None);
    };
    validate_task_id(task_id).map_err(|_| {
        Error::Other(format!(
            "transition marker at {path} holds an invalid task id ({task_id:?}); \
             leaving it in place"
        ))
    })?;
    let target = lines.next().unwrap_or("");
    if !is_valid_status_token(target) {
        return Err(Error::Other(format!(
            "transition marker at {path} holds an invalid target status ({target:?}); \
             leaving it in place"
        )));
    }
    Ok(Some(TransitionRequest {
        task_id: task_id.to_string(),
        target: target.to_string(),
    }))
}

/// Conservative shape check for a transition marker's target token: non-empty,
/// bounded, and limited to the characters status ids and the `reject`/`bounce`
/// verbs use (ASCII alphanumerics plus `-` and `_`). Defense-in-depth — the
/// token is never shell-interpolated — but it keeps a hostile or torn body from
/// reaching the decision path or the events log.
fn is_valid_status_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Remove the transition marker (idempotent — `rm -f` succeeds if absent). The
/// `rm -f` is identical to [`clear_ready_marker`]'s; kept as a distinct name so
/// call sites read as transition-vs-ready explicitly. Called once the poller
/// has consumed the request, and at task start to clear any stale marker before
/// the worktree is reused.
pub fn clear_transition_marker(host: &Host, marker: &Path) -> Result<()> {
    clear_ready_marker(host, marker)
}

/// Outcome of [`rebase_workspace_branch_onto_default`]. Pure data — the caller
/// decides what to log and whether to surface a warning to the user. We
/// distinguish "rebase wasn't needed" from "rebase succeeded" so the
/// events.log line can call out the actually-rebased commits when there
/// are any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Default branch is already an ancestor of the workspace's HEAD — the
    /// branch is up to date and no rewrite was needed.
    AlreadyUpToDate { default_sha: String },
    /// Rebase finished cleanly; the worktree's HEAD is now on top of
    /// `default_sha` (and its branch moved too when HEAD was attached).
    /// `before_sha`/`after_sha` are HEAD before/after the rewrite — equal
    /// when the rebase ran but produced an empty result (rare; harmless).
    Rebased {
        before_sha: String,
        after_sha: String,
        default_sha: String,
    },
    /// Rebase ran into conflicts. We aborted it so the worktree returned to
    /// a clean state at its exact original HEAD. A failed or inexact abort is
    /// reported as [`RebaseOutcome::Skipped`] instead.
    Conflict {
        default_sha: String,
        stderr_excerpt: String,
        /// Paths git left in a conflicted (unmerged) state, captured before
        /// the abort. Empty when git couldn't enumerate them. Callers that
        /// report conflicts machine-readably (e.g. `zen probe`'s
        /// `rebase_conflict`) surface this list.
        files: Vec<String>,
    },
    /// The rebase couldn't even be attempted (default branch missing,
    /// uncommitted changes that aren't ours to absorb, git itself errored).
    /// The reason field explains why so the events.log row is actionable.
    Skipped { reason: String },
}

impl RebaseOutcome {
    /// Short status token for the `events.log` `status=` field. Stable
    /// over time — UI consumers parse this.
    pub fn status_token(&self) -> &'static str {
        match self {
            RebaseOutcome::AlreadyUpToDate { .. } => "up-to-date",
            RebaseOutcome::Rebased { .. } => "ok",
            RebaseOutcome::Conflict { .. } => "conflict",
            RebaseOutcome::Skipped { .. } => "skipped",
        }
    }

    /// Compact one-line detail string for the `events.log` `detail=`
    /// field. Short SHAs on the happy paths, a snippet of git's stderr
    /// (or the reason) on the failure paths.
    pub fn detail(&self) -> String {
        fn short(sha: &str) -> &str {
            sha.get(..7).unwrap_or(sha)
        }
        match self {
            RebaseOutcome::AlreadyUpToDate { default_sha } => {
                format!("default={}", short(default_sha))
            }
            RebaseOutcome::Rebased {
                before_sha,
                after_sha,
                default_sha,
            } => format!(
                "{}..{}_onto_{}",
                short(before_sha),
                short(after_sha),
                short(default_sha),
            ),
            RebaseOutcome::Conflict {
                default_sha,
                stderr_excerpt,
                ..
            } => {
                let excerpt = stderr_excerpt.trim();
                if excerpt.is_empty() {
                    format!("default={}", short(default_sha))
                } else {
                    format!("default={} {}", short(default_sha), excerpt)
                }
            }
            RebaseOutcome::Skipped { reason } => reason.clone(),
        }
    }
}

/// Rebase `worktree`'s current HEAD onto `default_branch`. An attached HEAD
/// advances its branch; a detached HEAD remains detached at the rebased tip.
///
/// Why this exists: when a prereq task lands on `main` after a workspace has
/// already started its own task, the workspace's branch sits one or more
/// commits behind main by the time the review marker fires. Without this
/// hook the user had to drop into the workspace's worktree and run
/// `git rebase main` themselves before the review checkout would surface a
/// clean diff — exactly the manual rebase + force-push the task title
/// names. Running it here eliminates that step on the happy path, and
/// fails safely (worktree returned to its pre-rebase state) when a
/// conflict means a human has to step in anyway.
///
/// The worktree shares its `.git` with the project's main clone via
/// `git worktree add`, so `default_branch` already reflects whatever the
/// orchestrator merged onto it on the hub — no fetch is needed.
///
/// Contract:
///
/// - Worktree must be clean (anything outside `.claude/` would be lost
///   to a rebase conflict). The workspace is expected to have committed
///   before writing the review marker; a dirty worktree returns
///   [`RebaseOutcome::Skipped`].
/// - On conflict we require `git rebase --abort` to restore the exact original
///   HEAD before returning [`RebaseOutcome::Conflict`]. Otherwise the outcome
///   is `Skipped`, so no caller can consume a partial rebase as a valid tip.
/// - Never panics; every git failure surfaces as `Skipped` or `Conflict`
///   so the caller can log it and continue the review promotion.
pub fn rebase_workspace_branch_onto_default(
    host: &Host,
    worktree: &Path,
    default_branch: &str,
) -> RebaseOutcome {
    let wt_str = worktree.to_string_lossy().into_owned();

    // 1. Bail on a dirty worktree, but ignore shelbi's own footprint:
    //    `.claude/` (deploy: settings.json, the review marker) and `.shelbi/`
    //    (runtime scratch: `.shelbi/messages/` inter-agent mail). Both are
    //    normally gitignored, but a repo that hasn't ignored them yet still
    //    shouldn't have its post-task rebase skipped by our own files — the
    //    real bug this guards against was a rebase reporting
    //    `dirty_worktree(3_entries)` where all 3 were `.shelbi/messages/*`.
    let dirty = match shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "status", "--porcelain", "-z"],
    ) {
        Ok(s) => s,
        Err(e) => {
            return RebaseOutcome::Skipped {
                reason: format!("git_status_failed:{e}"),
            };
        }
    };
    let user_dirty = user_dirty_porcelain_lines(&dirty);
    if !user_dirty.is_empty() {
        return RebaseOutcome::Skipped {
            reason: format!("dirty_worktree({}_entries)", user_dirty.len()),
        };
    }

    // 2. Resolve the default branch ref. If the workspace's repo doesn't even
    //    know about `default_branch` (fresh clone never fetched, name
    //    typo'd in project YAML), there's nothing to rebase onto.
    let default_sha = match shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            &wt_str,
            "rev-parse",
            "--verify",
            &format!("{default_branch}^{{commit}}"),
        ],
    ) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(_) | Err(_) => {
            return RebaseOutcome::Skipped {
                reason: format!("default_branch_{default_branch}_not_found"),
            };
        }
    };

    let before_sha =
        match shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "rev-parse", "HEAD"]) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                return RebaseOutcome::Skipped {
                    reason: format!("rev_parse_HEAD_failed:{e}"),
                };
            }
        };

    // 3. Already a descendant? `merge-base --is-ancestor` exits 0 when
    //    `default_branch` is reachable from HEAD — i.e. the workspace's
    //    branch already contains every commit on main. No rewrite needed.
    let ancestor = shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            &wt_str,
            "merge-base",
            "--is-ancestor",
            default_branch,
            "HEAD",
        ],
    );
    if matches!(ancestor, Ok(ref o) if o.status.success()) {
        return RebaseOutcome::AlreadyUpToDate { default_sha };
    }

    // 4. Run the rebase. No autostash (we already proved the worktree is
    //    clean), no `--rebase-merges` (workspaces produce linear branches),
    //    and explicitly no `--update-refs`. The latter prevents a user's
    //    `rebase.updateRefs=true` setting from rewriting sibling task refs;
    //    callers that intentionally advance another durable ref do so with
    //    their own compare-and-swap after the rebase succeeds.
    let out = match shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            &wt_str,
            "rebase",
            "--no-update-refs",
            default_branch,
        ],
    ) {
        Ok(o) => o,
        Err(e) => {
            return RebaseOutcome::Skipped {
                reason: format!("rebase_spawn_failed:{e}"),
            };
        }
    };

    if !out.status.success() {
        // Conflict (or some other rebase-time error). Capture the unmerged
        // paths *before* aborting — `git rebase --abort` rolls the worktree
        // back and forgets them. `--diff-filter=U` lists exactly the files
        // left in a conflicted state.
        let files: Vec<String> = shelbi_ssh::run_capture(
            host,
            [
                "git",
                "-C",
                &wt_str,
                "diff",
                "--name-only",
                "--diff-filter=U",
            ],
        )
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{stderr}{stdout}");
        let excerpt = combined
            .lines()
            .find(|l| {
                let lc = l.to_ascii_lowercase();
                lc.contains("conflict") || lc.contains("error")
            })
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| {
                combined
                    .lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty())
                    .unwrap_or("rebase failed")
                    .to_string()
            });

        // Abort must restore the exact pre-rebase commit before we can call
        // this a safely-contained conflict. A detached rebase can leave HEAD
        // on the new base or a partially replayed commit while conflicted;
        // treating a failed abort as an ordinary conflict would let callers
        // mistake that partial result for a usable branch tip.
        let abort = match shelbi_ssh::run(host, ["git", "-C", &wt_str, "rebase", "--abort"]) {
            Ok(abort) => abort,
            Err(e) => {
                return RebaseOutcome::Skipped {
                    reason: format!("rebase_abort_spawn_failed:{e}"),
                }
            }
        };
        if !abort.status.success() {
            let abort_stderr = String::from_utf8_lossy(&abort.stderr);
            let detail = abort_stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("git rebase --abort failed");
            return RebaseOutcome::Skipped {
                reason: format!("rebase_abort_failed:{detail}"),
            };
        }
        let restored_head = match shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "rev-parse", "HEAD"],
        ) {
            Ok(sha) => sha.trim().to_string(),
            Err(e) => {
                return RebaseOutcome::Skipped {
                    reason: format!("post_abort_rev_parse_HEAD_failed:{e}"),
                }
            }
        };
        if restored_head != before_sha {
            return RebaseOutcome::Skipped {
                reason: format!(
                    "rebase_abort_restored_wrong_HEAD(expected_{before_sha},found_{restored_head})"
                ),
            };
        }
        return RebaseOutcome::Conflict {
            default_sha,
            stderr_excerpt: excerpt,
            files,
        };
    }

    let after_sha = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    RebaseOutcome::Rebased {
        before_sha,
        after_sha,
        default_sha,
    }
}

/// Argv for the local slot-liveness probe: list the project session's
/// windows so the caller can look for the workspace's window among them.
fn list_windows_argv(addr: &TmuxAddr) -> Vec<String> {
    vec![
        "tmux".into(),
        "list-windows".into(),
        "-t".into(),
        format!("={}", addr.session),
        "-F".into(),
        "#W".into(),
    ]
}

/// Does the workspace have a live tmux pane right now?
pub fn workspace_pane_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    // Local: check `session:window` exists. Remote: it's a whole session.
    // `tmux list-windows -t session -F #W | grep -w window` does both.
    let out = shelbi_ssh::run(host, list_windows_argv(addr)).map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|w| w.trim() == addr.window))
}

/// Does this workspace slot have a live tmux allocation right now?
///
/// Local workspaces are windows inside the project session, so the window is
/// the slot. Remote workspaces are standalone `shelbi-w-<name>` sessions; tmux
/// may auto-rename the lone window away from `agent`, so session liveness is
/// the authoritative availability check there.
pub fn workspace_slot_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    match host {
        Host::Local => workspace_pane_alive(host, addr),
        Host::Ssh { .. } => shelbi_tmux::has_session(host, &addr.session),
    }
}

/// tmux user option marking a workspace slot as a plain user shell opened
/// from the sidebar (`shelbi open` on an idle workspace) rather than a
/// shelbi-managed agent pane. Window-scoped for local workspaces (the
/// window is the slot), session-scoped for remote ones (the session is the
/// slot), so the mark dies with the slot and needs no cleanup path.
/// Dispatch reads it to refuse clobbering the user's shell;
/// `shelbi workspace list` reads it to render the slot as user-occupied
/// instead of an orphaned session.
pub const USER_SHELL_OPTION: &str = "@shelbi-user-shell";

/// Stamp the workspace's live tmux slot as a user shell (see
/// [`USER_SHELL_OPTION`]). Called right after the shell pane/session is
/// created by the open-idle-workspace path.
pub fn mark_user_shell(host: &Host, addr: &TmuxAddr) -> Result<()> {
    let argv: Vec<String> = match host {
        Host::Local => vec![
            "tmux".into(),
            "set-option".into(),
            "-w".into(),
            "-t".into(),
            shelbi_tmux::command_target(addr),
            USER_SHELL_OPTION.into(),
            "1".into(),
        ],
        Host::Ssh { .. } => vec![
            "tmux".into(),
            "set-option".into(),
            "-t".into(),
            format!("={}", addr.session),
            USER_SHELL_OPTION.into(),
            "1".into(),
        ],
    };
    shelbi_ssh::run_capture(host, &argv)?;
    Ok(())
}

/// Is this workspace slot occupied by a user shell — a live slot carrying
/// the [`USER_SHELL_OPTION`] mark? `false` for a dead slot, a live agent
/// pane, or an orphaned session (only the sidebar's open-idle-shell path
/// sets the mark). Dispatch uses this to skip the workspace while the
/// user is in it; the shell exiting tears the slot (and the mark) down,
/// returning the workspace to dispatchable.
pub fn workspace_user_shell_open(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    if !workspace_slot_alive(host, addr)? {
        return Ok(false);
    }
    let out = shelbi_ssh::run(host, user_shell_probe_argv(host, addr)).map_err(Error::Io)?;
    Ok(user_shell_mark_set(&out))
}

/// Argv reading the [`USER_SHELL_OPTION`] mark off a live slot — window-scoped
/// for local workspaces, session-scoped for remote ones.
fn user_shell_probe_argv(host: &Host, addr: &TmuxAddr) -> Vec<String> {
    match host {
        Host::Local => vec![
            "tmux".into(),
            "show-options".into(),
            "-w".into(),
            "-v".into(),
            "-t".into(),
            shelbi_tmux::command_target(addr),
            USER_SHELL_OPTION.into(),
        ],
        Host::Ssh { .. } => vec![
            "tmux".into(),
            "show-options".into(),
            "-v".into(),
            "-t".into(),
            format!("={}", addr.session),
            USER_SHELL_OPTION.into(),
        ],
    }
}

/// Did the user-shell option probe report the mark as set? A non-zero exit is
/// a plain "not marked" — older tmux exits non-zero for an unset user option —
/// not an error worth surfacing.
fn user_shell_mark_set(out: &std::process::Output) -> bool {
    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "1"
}

/// Result of a *bounded* workspace-slot probe — the variant of
/// [`workspace_slot_alive`] + [`workspace_user_shell_open`] used by
/// `shelbi workspace list` / `status --full`, where a machine that wedges
/// mid-handshake (e.g. Tailscale SSH parked on its web-auth prompt) must
/// degrade to an `unreachable` row instead of hanging the whole command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotProbe {
    /// The machine answered: no live tmux allocation for this slot.
    Dead,
    /// The machine answered: the slot has a live tmux allocation.
    Alive {
        /// The slot carries the [`USER_SHELL_OPTION`] mark.
        user_shell: bool,
    },
    /// The machine could not be asked — probe timed out or the transport
    /// failed. `reason` is a one-line human-readable cause for the row.
    Unreachable { reason: String },
}

/// Default wall-clock budget for one bounded slot probe. `ConnectTimeout=5`
/// already bounds a dead host, so this only has to catch the post-connect
/// wedges (interactive auth interception); 5s keeps `workspace list` well
/// under ~10s even with an extra machine, since unreachability is cached
/// per machine by the caller.
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 5_000;

/// The probe deadline, env-overridable via `SHELBI_PROBE_TIMEOUT_MS`
/// (clamped 500..=60_000) so a chronically slow link can be given a longer
/// leash without a rebuild.
pub fn probe_deadline() -> std::time::Duration {
    let ms = std::env::var("SHELBI_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_PROBE_TIMEOUT_MS)
        .clamp(500, 60_000);
    std::time::Duration::from_millis(ms)
}

/// Probe the workspace slot with a wall-clock deadline on every remote
/// command, classifying failures instead of propagating them: the caller
/// renders a table row per outcome and must never wedge or abort the whole
/// listing because one machine is down.
pub fn probe_workspace_slot(
    host: &Host,
    addr: &TmuxAddr,
    deadline: std::time::Duration,
) -> SlotProbe {
    let alive = match host {
        Host::Local => {
            // Same semantics as `workspace_pane_alive`: a non-zero exit is
            // "no session" (dead), success means look for the window.
            match shelbi_ssh::run_with_deadline(host, list_windows_argv(addr), deadline) {
                Ok(out) => {
                    out.status.success()
                        && String::from_utf8_lossy(&out.stdout)
                            .lines()
                            .any(|w| w.trim() == addr.window)
                }
                Err(e) => {
                    return SlotProbe::Unreachable {
                        reason: probe_error_reason(&e, deadline),
                    }
                }
            }
        }
        Host::Ssh { .. } => {
            let argv = vec![
                "tmux".to_string(),
                "has-session".to_string(),
                "-t".to_string(),
                format!("={}", addr.session),
            ];
            match shelbi_ssh::run_with_deadline(host, argv, deadline) {
                // Same discrimination as `shelbi_tmux::has_session`: tmux
                // answers 0 (exists) or 1 (doesn't, incl. no server); any
                // other exit is the transport failing, not tmux answering.
                Ok(out) => match out.status.code() {
                    Some(0) => true,
                    Some(1) => false,
                    _ => {
                        return SlotProbe::Unreachable {
                            reason: transport_failure_reason(&out),
                        }
                    }
                },
                Err(e) => {
                    return SlotProbe::Unreachable {
                        reason: probe_error_reason(&e, deadline),
                    }
                }
            }
        }
    };
    if !alive {
        return SlotProbe::Dead;
    }
    // Best-effort mark probe, same degradation as the unbounded path: an
    // unreadable option reads as "not a user shell", never an error. The
    // machine just answered the liveness probe, so a timeout here is a
    // blip, not the auth wedge — degrading beats flapping to unreachable.
    let mark_argv = user_shell_probe_argv(host, addr);
    let user_shell = match shelbi_ssh::run_with_deadline(host, mark_argv, deadline) {
        Ok(out) => user_shell_mark_set(&out),
        Err(_) => false,
    };
    SlotProbe::Alive { user_shell }
}

/// One-line reason for a probe that never produced an exit status. The
/// timeout case is worded for its dominant cause — an SSH session parked on
/// an interactive auth step that BatchMode can't suppress (Tailscale SSH's
/// web-auth flow runs outside the openssh client).
fn probe_error_reason(e: &std::io::Error, deadline: std::time::Duration) -> String {
    if e.kind() == std::io::ErrorKind::TimedOut {
        format!(
            "ssh probe timed out after {}s (interactive auth pending?)",
            deadline.as_secs()
        )
    } else {
        format!("probe failed: {e}")
    }
}

/// One-line reason for a probe whose transport answered with a non-tmux
/// exit (e.g. ssh's 255): prefer ssh's own first diagnostic line.
fn transport_failure_reason(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    match stderr.lines().find(|l| !l.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => format!("ssh exited {}", out.status),
    }
}

/// Kill the workspace's pane (idempotent — silently OK if already gone).
///
/// Marks an "expected teardown" for `workspace_name` before touching tmux
/// so the local pane's lifecycle wrapper (`shelbi open <name> --as-pane`)
/// can distinguish shelbi-initiated shutdowns from real pane deaths.
/// Without the mark, `tmux kill-window` delivers SIGHUP to the wrapper,
/// which would emit `project=<name> workspace=<name> pane_alive=false reason=signal:SIGHUP`
/// to events.log even when the caller is a normal dispatch — spuriously
/// tripping the orchestrator's "pane died, surface to user" reaction rule
/// right before the replacement pane comes up. See
/// bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch.
pub fn kill_workspace_pane(host: &Host, addr: &TmuxAddr, workspace_name: &str) -> Result<()> {
    // Local: `kill-window -t session:window` (the dashboard session
    // must stay alive). Remote: `kill-session -t session` (the session
    // IS the workspace).
    //
    // The liveness check has to differ too. For local we look for the
    // workspace's window inside the shared dashboard session. For remote
    // we look for the session itself — NOT for a window named `agent`
    // — because tmux's `automatic-rename` (on by default) renames the
    // window after whatever command is running (`claude`, `bash`, …),
    // and a window-name match would miss live sessions and leave them
    // around to collide with the next `task start`.
    match host {
        Host::Local => {
            if !workspace_slot_alive(host, addr)? {
                return Ok(());
            }
            // Best-effort — the wrapper's fallback (fire the event with
            // its historical reason) is the pre-fix behavior, so a mark
            // failure just degrades to that.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let target = shelbi_tmux::command_target(addr);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &target])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            if !workspace_slot_alive(host, addr)? {
                return Ok(());
            }
            // Remote workspaces don't run the lifecycle wrapper (no
            // shelbi binary on the workspace host), so there's nothing
            // to suppress on that side — but writing the marker is
            // still safe and keeps the API symmetric.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let target = format!("={}", addr.session);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &target])
                .map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Spec for `start_workspace_on_task`. We don't take a `&Task` because the
/// caller may have a fresh task id without a frontmatter file yet.
pub struct StartSpec<'a> {
    pub project: &'a Project,
    pub workspace: &'a WorkspaceSpec,
    pub task_id: &'a str,
    pub branch: &'a str,
    /// Body of the task markdown — appended to the prompt as context.
    pub task_body: &'a str,
    /// Name of the agent (under `agents/<name>/`) whose `instructions.md`
    /// is wired up as the runner's system prompt and whose `skills/` dir
    /// is mounted into the worktree's `.claude/skills/`. `None` skips
    /// the agent-context deploy entirely — kept optional so non-CLI
    /// callers that don't yet resolve through
    /// [`crate::dispatch::resolve_dispatch_agent`] (or tests that
    /// exercise the spawn path in isolation) can opt out without
    /// fabricating an agent name.
    pub agent: Option<&'a str>,
}

/// Tear down the workspace's pane, switch its worktree to `branch` (creating
/// the worktree off `default_branch` and the branch off `default_branch` if
/// needed), and start the runner with an initial prompt. Bails on a dirty
/// worktree so the user doesn't silently lose work.
pub fn start_workspace_on_task(spec: StartSpec<'_>) -> Result<TmuxAddr> {
    let machine = spec
        .project
        .machine(&spec.workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.workspace.machine.clone()))?
        .clone();
    let runner = spec
        .project
        .runner(&spec.workspace.runner)
        .ok_or_else(|| Error::UnknownRunner(spec.workspace.runner.clone()))?
        .clone();

    let host = machine.host();
    let worktree = workspace_worktree(&machine, spec.workspace);
    let addr = workspace_tmux_addr(spec.project, spec.workspace)?;

    // 0. Serialize the whole dispatch against any concurrent start for the
    //    same workspace. Without this, two `task start`s racing one
    //    workspace interleave sync-worktree / checkout / pane-recreate and
    //    leave the pane running one branch while the worktree sits on
    //    another. The guard is held until this function returns.
    let _dispatch_lock = shelbi_state::lock_workspace(&spec.project.name, &spec.workspace.name)?;

    // 0a. If the project asks for auto-mode, claude must be v2.1.83+. Older
    //     versions silently fall back to `default` and the user gets a Bash
    //     prompt on every command — exactly the bug we're trying to avoid.
    //     Surface it up front so the failure mode is "shelbi rejected this
    //     machine" instead of "my workspace keeps pausing for no reason."
    require_auto_mode_supported(&host, &runner, &spec.project.workspace_permissions_mode)?;

    // 0b. Clear any stale review marker left in the worktree from a previous
    //     task before we reuse the worktree — otherwise the poller could read
    //     an old task id and misfire. Best-effort: a failure here shouldn't
    //     block standing up the workspace.
    let marker = workspace_ready_marker(&machine, spec.workspace);
    let _ = clear_ready_marker(&host, &marker);
    // Same for any stale agent-transition marker — a fresh dispatch must not
    // inherit a bounce request left behind by the previous task on this worktree.
    let _ = clear_transition_marker(
        &host,
        &workspace_transition_marker(&machine, spec.workspace),
    );

    // 1. Make sure the worktree exists + is on the right branch, clean.
    //    Lock order is workspace -> Git worktrees/refs everywhere. Hold the
    //    project-wide Git lock through the post-checkout verification so a
    //    Zen probe cannot rewrite the ref between checkout and verification.
    //    It must be dropped before pane startup: the pane wrapper may call
    //    ensure_workspace_worktree while this function waits for readiness.
    let git_worktree_lock = shelbi_state::lock_git_worktrees(&spec.project.name)?;
    //    A failure here (dirty worktree, unreachable origin during the
    //    fresh-cut fetch, git error) aborts the dispatch before any pane
    //    is touched — surface it in events.log so `shelbi events tail`
    //    and the orchestrator see the stall, not just the CLI caller.
    if let Err(e) = sync_worktree(
        spec.project,
        &host,
        &machine,
        &worktree,
        spec.branch,
        spec.project.base_branch(),
    ) {
        if let Err(log_err) = shelbi_state::append_dispatch_event(
            spec.task_id,
            &spec.workspace.name,
            "sync-failed",
            &e.to_string(),
        ) {
            eprintln!("shelbi: failed to record dispatch sync failure in events.log: {log_err}");
        }
        return Err(e);
    }

    // 1b. Branch-discipline check (bug-worker-commit-landed-on-hub-main-
    //     checkout): the launch cwd's HEAD must be attached to the task
    //     branch before any agent starts. `sync_worktree` is supposed to
    //     guarantee this, so a mismatch means something raced or healed
    //     wrong — abort the dispatch rather than launch an agent whose
    //     commits would land on the wrong ref.
    if let Err(e) = verify_worktree_on_branch(&host, &worktree, spec.branch) {
        if let Err(log_err) = shelbi_state::append_dispatch_event(
            spec.task_id,
            &spec.workspace.name,
            "branch-mismatch",
            &e.to_string(),
        ) {
            eprintln!("shelbi: failed to record dispatch branch mismatch in events.log: {log_err}");
        }
        return Err(e);
    }
    drop(git_worktree_lock);

    // The worktree is now clean and ready, but the old pane is still intact.
    // Validate the runner here so a missing `codex`/`claude` binary is an
    // actionable dispatch error rather than a pane that starts and exits
    // before Shelbi can deliver its startup prompt.
    require_runner_available(&host, &runner)?;

    // 2–7. Deploy the agent context, reset the pane, launch the runner, wait
    //       for readiness, and send the loop-closing dev prompt. Dev
    //       workspaces inject no `PORT` (that's a review-workspace concern).
    let prompt = compose_prompt(
        spec.task_id,
        spec.branch,
        spec.task_body,
        &marker,
        &spec.project.default_branch,
        &spec.project.name,
        shelbi_agent::polls_for_messages(&runner),
    );
    deploy_and_spawn(SpawnArgs {
        project: spec.project,
        workspace: spec.workspace,
        runner: &runner,
        host: &host,
        worktree: &worktree,
        addr: &addr,
        task_id: spec.task_id,
        agent: spec.agent,
        port: None,
        resume: false,
        prompt: &prompt,
    })?;

    Ok(addr)
}

/// Relaunch a workspace on the task it is ALREADY working, without discarding
/// the in-flight worktree — the recovery sibling of [`start_workspace_on_task`]
/// (`shelbi task resume`). Where `start` clears context (kills the pane and
/// re-checks-out a clean branch, right for a fresh dispatch), `resume` is for a
/// stalled or killed worker: the tmux session was killed, the pane wedged, or
/// the agent stopped mid-task, and we want it going again on the SAME task with
/// its work intact.
///
/// The differences from the dev-start path, all in service of "don't lose the
/// in-flight state":
///
/// - **Worktree preserved as-is.** [`sync_worktree_for_resume`] never bails on
///   a dirty worktree and never resets or re-checks-out the branch — the
///   branch, its commits, and any uncommitted changes stay exactly where the
///   worker left them. It only *recreates* the worktree when it's missing
///   entirely (e.g. a prior teardown removed it), checking out the existing
///   task branch.
/// - **Conversation resumed when the runner supports it.** For a claude runner
///   the pane relaunches with `--continue` (`resume: true` below → the pane
///   wrapper / remote launch add the flag), so the worker picks up its prior
///   conversation with full context rather than reading its own code cold. For
///   every other runner we fall back to re-injecting the task prompt — the
///   agent continues by reading the work already in its worktree.
///
/// The pane is still recreated (that's how a killed/wedged session is
/// reclaimed — the `duplicate session` case is handled by
/// [`kill_workspace_pane`] inside `deploy_and_spawn`, which tears down any
/// stale pane before the fresh one comes up), and the dispatch is still
/// serialized against concurrent starts for the same workspace.
pub fn resume_workspace_on_task(spec: StartSpec<'_>) -> Result<TmuxAddr> {
    let machine = spec
        .project
        .machine(&spec.workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.workspace.machine.clone()))?
        .clone();
    let runner = spec
        .project
        .runner(&spec.workspace.runner)
        .ok_or_else(|| Error::UnknownRunner(spec.workspace.runner.clone()))?
        .clone();

    let host = machine.host();
    let worktree = workspace_worktree(&machine, spec.workspace);
    let addr = workspace_tmux_addr(spec.project, spec.workspace)?;

    // Serialize against any concurrent start/resume for the same workspace —
    // same rationale as the dev path. Held until this function returns.
    let _dispatch_lock = shelbi_state::lock_workspace(&spec.project.name, &spec.workspace.name)?;

    require_auto_mode_supported(&host, &runner, &spec.project.workspace_permissions_mode)?;

    // Clear any stale review marker before we relaunch so the poller can't read
    // an old task id and misfire. A task being resumed is in `in_progress`, so
    // any marker present is stale. Best-effort.
    let marker = workspace_ready_marker(&machine, spec.workspace);
    let _ = clear_ready_marker(&host, &marker);
    // Same for any stale agent-transition marker — a resumed task is still
    // in-progress, so any transition request present is stale. Best-effort.
    let _ = clear_transition_marker(
        &host,
        &workspace_transition_marker(&machine, spec.workspace),
    );

    // Preserve the worktree: don't reset the branch, don't bail on a dirty
    // tree. Only recreate the worktree if it's gone entirely. A failure here
    // aborts before any pane is touched — surface it in events.log so the stall
    // is visible to `shelbi events tail` and the orchestrator, not just the CLI.
    // Lock order is workspace -> Git worktrees/refs. Drop the inner lock
    // before pane startup for the same wrapper-reentry reason as start.
    let git_worktree_lock = shelbi_state::lock_git_worktrees(&spec.project.name)?;
    if let Err(e) = sync_worktree_for_resume(
        &host,
        &machine,
        &worktree,
        spec.branch,
        spec.project.base_branch(),
    ) {
        if let Err(log_err) = shelbi_state::append_dispatch_event(
            spec.task_id,
            &spec.workspace.name,
            "resume-sync-failed",
            &e.to_string(),
        ) {
            eprintln!("shelbi: failed to record resume sync failure in events.log: {log_err}");
        }
        return Err(e);
    }
    drop(git_worktree_lock);

    // Prefer true conversation-resume for claude; every other runner falls back
    // to plain prompt re-injection (the agent reads its own prior work).
    let resume = matches!(
        shelbi_agent::RunnerAdapter::for_spec(&runner).resume_strategy(),
        shelbi_agent::ResumeStrategy::Transcript
    );
    let prompt = compose_resume_prompt(
        spec.task_id,
        spec.branch,
        spec.task_body,
        &marker,
        &spec.project.default_branch,
        &spec.project.name,
        shelbi_agent::polls_for_messages(&runner),
        resume,
    );
    deploy_and_spawn(SpawnArgs {
        project: spec.project,
        workspace: spec.workspace,
        runner: &runner,
        host: &host,
        worktree: &worktree,
        addr: &addr,
        task_id: spec.task_id,
        agent: spec.agent,
        port: None,
        resume,
        prompt: &prompt,
    })?;

    Ok(addr)
}

/// Inputs to [`deploy_and_spawn`] — the shared spawn tail for the dispatch
/// path ([`start_workspace_on_task`] / [`resume_workspace_on_task`]).
struct SpawnArgs<'a> {
    project: &'a Project,
    workspace: &'a WorkspaceSpec,
    runner: &'a shelbi_core::AgentRunnerSpec,
    host: &'a Host,
    worktree: &'a Path,
    addr: &'a TmuxAddr,
    task_id: &'a str,
    /// Agent whose context is deployed + wired as the runner's system
    /// prompt. `None` skips the agent-context deploy (embed tests).
    agent: Option<&'a str>,
    /// Injected as `PORT` into the pane env when `Some`. Currently always
    /// `None` on the dispatch path; retained so the pane-launch plumbing can
    /// export a slot-derived port without a signature change.
    port: Option<u16>,
    /// `true` for a `shelbi task resume`: the pane is relaunched WITHOUT
    /// clearing the worktree, and a claude runner reloads its prior
    /// conversation via `--continue`. `false` for a normal (context-clearing)
    /// dispatch.
    resume: bool,
    prompt: &'a str,
}

/// REPL-only non-Claude runners do not have a pane-chrome readiness parser
/// Shelbi can trust. Give their TUI a short, conservative startup window
/// before using the shared split text/Enter delivery path. Claude keeps its
/// stronger structural readiness probe below.
const NON_CLAUDE_PASTE_STARTUP_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

/// Steps 2–7 shared by both dispatch paths: deploy the agent's settings +
/// context, reset the pane, launch the runner, wait for the input box, send
/// `prompt`, and confirm submission. Returns `Ok(())` once submission is
/// confirmed; a readiness timeout or lost prompt records an actionable
/// dispatch event and returns `Err`, so the caller can leave the task put
/// for a retry.
fn deploy_and_spawn(a: SpawnArgs<'_>) -> Result<()> {
    // 2. Drop a rendered .claude/settings.json into the worktree so the
    //    runner picks up shelbi's window-title hooks (idle/working/blocked)
    //    and the per-task message-tail hooks (Phase 7 push delivery).
    //    Prefer the dispatched agent's `agents/<role>/settings.json` when
    //    present so role-specific hook customization actually takes effect
    //    on the worktree's Claude Code session; fall back to the
    //    project-wide template otherwise. Overwrite is fine — this is the
    //    entire on-workspace footprint and we re-render it on every start.
    let rendered = render_workspace_settings_preferring_agent(a.project, a.agent)?;
    deploy_workspace_settings(a.host, a.worktree, &rendered)?;
    deploy_runner_hooks(a.host, a.worktree)?;

    // Decide + record the hub→workspace message channel for THIS launch. The
    // mode is derived from the runner's verified hook health (see
    // `shelbi_agent::message_channel`), not assumed, and the event line makes
    // the choice auditable when a message goes undelivered — polling means the
    // prompt carries the pull contract; hooks means the pane pushes.
    let channel = shelbi_agent::message_channel(a.runner);
    let runner_name = std::path::Path::new(&a.runner.command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(a.runner.command.as_str());
    append_dispatch_status(
        a.task_id,
        &a.workspace.name,
        "message-channel",
        &format!("mode={} runner={}", channel.as_str(), runner_name),
    );

    // 2b. Deploy the dispatched agent's `instructions.md` + skills into the
    //     worktree's `.claude/` footprint. The instructions file becomes the
    //     runner's `--append-system-prompt` source (see step 4); the skills
    //     directory is wiped and re-mounted from `agents/<agent>/skills/` so
    //     consecutive dispatches with different agents on the same workspace
    //     don't accumulate skills from earlier runs. Skipped when `agent` is
    //     `None` (e.g. an embed test that exercises the spawn path without
    //     resolving an agent).
    if let Some(agent) = a.agent {
        deploy_agent_context(a.host, a.worktree, &a.project.name, agent)?;
    }

    let prompt_injection = effective_prompt_injection_for_spawn(a.runner, a.resume);
    let submit_profile = crate::submit::SubmitProfile::for_runner(a.runner);
    let launch_seed = prompt_injection.kind != PromptInjectionKind::Paste;
    let startup_prompt_rel = if launch_seed {
        let startup_prompt = render_startup_prompt(a.prompt, a.agent.is_some(), a.runner);
        deploy_startup_prompt(a.host, a.worktree, &startup_prompt)?;
        Some(WORKTREE_STARTUP_PROMPT_REL)
    } else {
        None
    };

    // 3. Reset the tmux pane — that's how we clear context. If it doesn't
    //    exist yet, this is a no-op; otherwise the next step recreates it.
    kill_workspace_pane(a.host, a.addr, &a.workspace.name)?;

    // 4 + 5. Create the pane and launch the agent.
    //
    // Local: the pane's top-level process is the `shelbi open
    //   <name> --as-pane` lifecycle wrapper. The wrapper cd's into the
    //   worktree, execs the agent, waits for it, and writes a
    //   `pane_alive=false reason=<…>` line to events.log on any exit
    //   path (clean exit, SIGHUP from tmux teardown, SIGTERM from
    //   kill-window, SIGINT, child crash). Same wrapper invocation as
    //   the sidebar-click path so a manual `tmux kill-window` and a
    //   workspace dispatch can't drift apart.
    //
    // Remote: the lifecycle wrapper isn't deployed to remote machines
    //   (no shelbi binary on the workspace host), so the historical
    //   `send_line(cd && claude)` flow stays. We still create an empty
    //   session first so the user's login rc files run when the shell
    //   spawns.
    //
    //   `LANG=C.UTF-8` is cheap, low-risk insurance: a non-interactive
    //   SSH launch can leave the tmux server in the C locale, and forcing
    //   UTF-8 keeps every box-drawing/glyph path well-defined regardless
    //   of host config.
    //
    //   The `$SHELL -lc` re-exec on the remote path is needed because
    //   tmux was started by `ssh host -- tmux new-session …`, which runs
    //   through a NON-login non-interactive shell — tmux (and every pane
    //   it spawns) inherits a stripped-down PATH missing Homebrew, asdf,
    //   nvm, etc. The login shell sources ~/.zprofile / ~/.bash_profile
    //   and picks up the same PATH the user has in their own terminal.
    match a.host {
        Host::Local => {
            let shelbi_bin = current_exe_string()?;
            // A resume re-enters the wrapper with `--resume` so it launches the
            // runner with `--continue` (claude) instead of a cold start — the
            // wrapper builds its launch command through the same
            // `workspace_launch_command` this path's remote arm calls, so the
            // resume flag has to reach it there too.
            let resume_flag = if a.resume { " --resume" } else { "" };
            let pane_cmd = format!(
                "{bin} --project {proj} open {ws} --as-pane{resume_flag}",
                bin = shelbi_agent::shell_escape(&shelbi_bin),
                proj = shelbi_agent::shell_escape(&a.project.name),
                ws = shelbi_agent::shell_escape(&a.workspace.name),
            );
            let hub_sock = shelbi_state::hub_socket_path()
                .map_err(|e| Error::Other(format!("resolving hub socket path: {e}")))?;
            let create_new_session = !shelbi_tmux::has_session(a.host, &a.addr.session)?;
            let argv = local_pane_tmux_argv(LocalPaneTmuxArgs {
                create_new_session,
                session: &a.addr.session,
                window: &a.addr.window,
                task_id: a.task_id,
                project: &a.project.name,
                workspace: &a.workspace.name,
                hub_sock: &hub_sock.to_string_lossy(),
                pane_cmd: &pane_cmd,
                port: a.port,
            });
            shelbi_ssh::run_capture(a.host, &argv).map_err(|e| {
                Error::Other(format!(
                    "pane startup failure for workspace `{}` using {} runner `{}`: {e}",
                    a.workspace.name,
                    runner_label(&a.runner.command),
                    a.runner.command,
                ))
            })?;
        }
        Host::Ssh { .. } => {
            shelbi_tmux::new_session(a.host, &a.addr.session, &a.addr.window, None).map_err(
                |e| {
                    Error::Other(format!(
                        "pane startup failure for workspace `{}` using {} runner `{}`: {e}",
                        a.workspace.name,
                        runner_label(&a.runner.command),
                        a.runner.command,
                    ))
                },
            )?;
            // Remote panes run the agent directly — the lifecycle wrapper isn't
            // deployed on the workspace host — so we build the launch command
            // here and send it into the pane. This goes through the SAME
            // `workspace_launch_command` constructor the local wrapper
            // (`shelbi open --as-pane`) uses, so the two host paths can't drift.
            let launch = workspace_launch_command_with_startup_prompt(
                a.runner,
                &a.project.workspace_permissions_mode,
                a.agent.is_some(),
                a.resume,
                startup_prompt_rel,
            );
            let cd_launch =
                remote_cd_launch(a.host, a.worktree, &launch, a.port, &a.workspace.name);
            shelbi_tmux::send_line(a.host, a.addr, &cd_launch).map_err(|e| {
                Error::Other(format!(
                    "pane startup failure for workspace `{}` using {} runner `{}`: {e}",
                    a.workspace.name,
                    runner_label(&a.runner.command),
                    a.runner.command,
                ))
            })?;
        }
    }

    if launch_seed {
        // The pane was just killed + recreated (step 3), so it carries no
        // scrollback from a prior run — any busy signal is genuinely this
        // dispatch. The fresh baseline keeps every submit signal live. The
        // prompt was seeded via the launch command itself, so the first pass
        // is verify-only (nothing to deliver).
        let baseline = crate::submit::PaneBaseline::fresh(submit_profile);
        let status = crate::submit::verify_submitted(a.host, a.addr, a.prompt, &baseline);
        if record_dispatch_submit(a.addr, a.task_id, &a.workspace.name, status) {
            return Ok(());
        }
        if shelbi_agent::RunnerAdapter::for_spec(a.runner).needs_claude_readiness_probe()
            && crate::ready::wait_for_claude_ready(a.host, a.addr, crate::ready::READY_TIMEOUT)?
        {
            let status = crate::submit::send_verified(a.host, a.addr, a.prompt, &baseline)?;
            if record_dispatch_submit(a.addr, a.task_id, &a.workspace.name, status) {
                return Ok(());
            }
        }
        return Err(Error::Other(format!(
            "dispatch to {} was not confirmed — no busy signal after launch-seed delivery",
            a.addr.target(),
        )));
    }

    // 6. For Claude, wait until its input box is structurally ready before
    //    typing the prompt. A fixed sleep is fragile: Claude's boot time
    //    varies with load, and on a freshly-created worktree it may interpose
    //    a "trust this folder" dialog first (which `wait_for_claude_ready`
    //    auto-confirms). Other runners must not be inspected with this
    //    Claude-specific parser; explicit `paste` runners use the conservative
    //    startup settle above, then the shared delivery-only submit profile.
    //
    //    If the probe times out we do NOT fire-and-forget. Typing into a
    //    not-yet-ready UI silently drops the whole prompt: the keystrokes land
    //    nowhere and claude sits at a fresh idle box, yet the old code sent
    //    anyway and the caller then marked the task `in_progress` — stranding a
    //    dead workspace "active" forever with no prompt (observed 2026-07-02 on
    //    alpha). Instead we abort the dispatch cleanly: record an actionable
    //    event and return an error so the caller leaves the task in its
    //    ready-category column for a clean retry.
    if submit_profile.uses_claude_ui() {
        if !crate::ready::wait_for_claude_ready(a.host, a.addr, crate::ready::READY_TIMEOUT)? {
            if let Err(e) = shelbi_state::append_dispatch_event(
                a.task_id,
                &a.workspace.name,
                "stalled",
                "readiness_timeout",
            ) {
                eprintln!("shelbi: failed to record dispatch stalled in events.log: {e}");
            }
            return Err(Error::Other(format!(
                "claude readiness probe timed out after {}s on {} — dispatch aborted, \
                 prompt NOT sent so the task stays put for retry. Check the workspace \
                 pane, then re-run the dispatch.",
                crate::ready::READY_TIMEOUT.as_secs(),
                a.addr.target(),
            )));
        }
    } else {
        std::thread::sleep(NON_CLAUDE_PASTE_STARTUP_SETTLE);
    }
    // Baseline the pane's pre-delivery state BEFORE we deliver the prompt. On
    // a claude `--continue` resume the pane replays the prior conversation
    // into the scrollback — token-usage footers (`… ↑ 3.2k tokens)`) and all —
    // so the busy probe would read that replay as "busy" and false-confirm a
    // prompt whose Enter was actually dropped (the workspace then sits idle at
    // Ctx 0 while the board shows `in_progress`; observed 2026-07-09 on bravo
    // after a `task resume`). The baseline tells the verifier which signals
    // predate this delivery so it can suppress them and rely on the reliable
    // ones (title flipping to `shelbi:working` / input box clearing).
    let baseline = crate::submit::PaneBaseline::capture(a.host, a.addr, submit_profile);

    // 7. Deliver the prompt and verify it actually got submitted, not just
    //    typed into the input box — `crate::submit::send_verified` sends the
    //    text, settles, sends Enter as a separate key event, and polls for a
    //    submit signal (retrying the Enter once if the prompt is visibly
    //    parked in the box).
    //
    //    If no signal ever lands, the prompt is lost — do not mark the task
    //    active. `record_dispatch_submit` records a `status=stalled` dispatch
    //    event; we then return an error so the caller leaves the task in its
    //    ready-category column, exactly like a readiness timeout, instead of
    //    moving it to `in_progress` on a workspace that never got the prompt.
    let status = crate::submit::send_verified(a.host, a.addr, a.prompt, &baseline)?;
    if !record_dispatch_submit(a.addr, a.task_id, &a.workspace.name, status) {
        return Err(Error::Other(format!(
            "prompt was not accepted on {} — no submission signal after a retry \
             Enter. Dispatch aborted so the task stays put for retry; check the \
             workspace pane.",
            a.addr.target(),
        )));
    }

    Ok(())
}

/// Map a verified-submit verdict onto the dispatch ledger: record the
/// `dispatch … status=confirmed/unverified/stalled` event (so `shelbi events
/// tail` and the orchestrator see it) and report whether the dispatch may
/// proceed. An unsupported runner probe is accepted as explicitly unverified:
/// the shared split text/Enter delivery ran, but Shelbi did not apply Claude's
/// pane parser to an unrelated TUI. The submit probing itself lives in
/// [`crate::submit`] — the same primitive every other pane-injection path
/// (`shelbi send`, supervision re-injection) routes through.
///
/// Both non-submitted verdicts collapse to the long-standing
/// `stalled detail=no_busy_signal_after_retry` event, which orchestrator
/// reaction rules already key on: whether the prompt is visibly parked in
/// the box or simply unproven, the dispatch must not mark the task active.
fn record_dispatch_submit(
    addr: &TmuxAddr,
    task_id: &str,
    workspace: &str,
    status: crate::submit::SubmitStatus,
) -> bool {
    match status {
        crate::submit::SubmitStatus::Submitted { detail } => {
            append_dispatch_status(task_id, workspace, "confirmed", detail);
            true
        }
        crate::submit::SubmitStatus::DeliveredUnverified { detail } => {
            append_dispatch_status(task_id, workspace, "unverified", detail);
            true
        }
        crate::submit::SubmitStatus::EligibilityRevoked
        | crate::submit::SubmitStatus::StillInBox
        | crate::submit::SubmitStatus::Unconfirmed => {
            eprintln!(
                "shelbi: dispatched prompt to {} but no submission signal appeared \
                 after a retry Enter — dispatch stalled; leaving the task unmoved",
                addr.target(),
            );
            append_dispatch_status(task_id, workspace, "stalled", "no_busy_signal_after_retry");
            false
        }
    }
}

/// Outcome of one auto-resume nudge on a usage-limit-stalled pane. Every
/// variant is a normal, recorded result — only transport errors surface as
/// `Err` from [`resume_limit_stalled_pane`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitResumeOutcome {
    /// The resume prompt was delivered and provably submitted.
    Submitted,
    /// The pane is no longer on the limit banner (or is actively working) —
    /// someone already resumed it, so we touched nothing.
    SkippedBannerGone,
    /// A newer structurally valid banner replaced the scheduled incident.
    SkippedIncidentChanged,
    /// The workspace no longer owns the scheduled active Claude task.
    SkippedIneligible,
    /// The limit modal never gave way to a ready input box — the window
    /// likely hasn't actually reset yet. Nothing was typed.
    InputNotReady,
    /// The modal disappeared without exposing a ready input box.
    DeliveryUncertain,
    /// Ownership changed while the delivered prompt was still parked, so
    /// the guarded shared submit primitive withheld its retry Enter.
    PromptParkedIneligible,
    /// The prompt was typed but no submission signal appeared even after the
    /// retry Enter — it may be sitting unsubmitted in the input box.
    SubmitUnconfirmed,
}

/// Nudge a worker pane stalled on claude's usage/session-limit modal back to
/// work, once its limit window has reset. Called by the hub poller when a
/// scheduled resume comes due (see the poller's limit-resume state machine).
///
/// The sequence mirrors what a human does at the pane, wrapped in the same
/// safeguards the dispatch path uses:
///
/// 1. Re-verify the exact scheduled stall on a fresh capture. A missing or
///    different current banner means a manual resume or newer incident beat
///    us. Check `is_eligible` immediately before the first pane mutation.
/// 2. Enter selects the modal's default option (`Stop and wait for limit to
///    reset`), which returns claude to its input box once the window has
///    reset; then wait for readiness exactly like dispatch does.
/// 3. Re-check `is_eligible` after readiness and immediately before delivery.
/// 4. Deliver `prompt` and verify it provably submitted (title marker, busy
///    signal, or the input box clearing — with one retry Enter), via the
///    guarded form of the same [`crate::submit::send_verified`] primitive
///    dispatch uses. Its pre-delivery baseline suppresses stale busy signals
///    from the prior conversation's scrollback, and its guard is re-checked
///    before a retry Enter.
pub fn resume_limit_stalled_pane<F>(
    host: &Host,
    addr: &TmuxAddr,
    expected_stall: &crate::ready::UsageLimitStall,
    prompt: &str,
    is_eligible: F,
) -> Result<LimitResumeOutcome>
where
    F: Fn() -> bool,
{
    let screen = shelbi_tmux::capture(host, addr)?;
    match classify_limit_resume_screen(&screen, expected_stall) {
        LimitResumeScreen::ExpectedIncident => {}
        LimitResumeScreen::BannerGone => return Ok(LimitResumeOutcome::SkippedBannerGone),
        LimitResumeScreen::IncidentChanged => {
            return Ok(LimitResumeOutcome::SkippedIncidentChanged);
        }
    }
    if !is_eligible() {
        return Ok(LimitResumeOutcome::SkippedIneligible);
    }
    shelbi_tmux::send_enter(host, addr)?;
    if !crate::ready::wait_for_claude_ready(host, addr, crate::ready::READY_TIMEOUT)? {
        let after_wait = shelbi_tmux::capture(host, addr)?;
        return Ok(
            if classify_limit_resume_screen(&after_wait, expected_stall)
                == LimitResumeScreen::ExpectedIncident
            {
                LimitResumeOutcome::InputNotReady
            } else {
                LimitResumeOutcome::DeliveryUncertain
            },
        );
    }
    let baseline =
        crate::submit::PaneBaseline::capture(host, addr, crate::submit::SubmitProfile::ClaudeUi);
    if !is_eligible() {
        return Ok(LimitResumeOutcome::SkippedIneligible);
    }
    match crate::submit::send_verified_guarded(host, addr, prompt, &baseline, is_eligible)? {
        crate::submit::SubmitStatus::Submitted { .. } => Ok(LimitResumeOutcome::Submitted),
        crate::submit::SubmitStatus::EligibilityRevoked => {
            Ok(LimitResumeOutcome::PromptParkedIneligible)
        }
        crate::submit::SubmitStatus::DeliveredUnverified { .. }
        | crate::submit::SubmitStatus::StillInBox
        | crate::submit::SubmitStatus::Unconfirmed => Ok(LimitResumeOutcome::SubmitUnconfirmed),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitResumeScreen {
    ExpectedIncident,
    BannerGone,
    IncidentChanged,
}

fn classify_limit_resume_screen(
    screen: &str,
    expected_stall: &crate::ready::UsageLimitStall,
) -> LimitResumeScreen {
    match crate::ready::detect_usage_limit(screen) {
        None => LimitResumeScreen::BannerGone,
        Some(current) if current == *expected_stall => LimitResumeScreen::ExpectedIncident,
        Some(_) => LimitResumeScreen::IncidentChanged,
    }
}

fn append_dispatch_status(task_id: &str, workspace: &str, status: &str, detail: &str) {
    if let Err(e) = shelbi_state::append_dispatch_event(task_id, workspace, status, detail) {
        eprintln!("shelbi: failed to record dispatch {status} in events.log: {e}");
    }
}

/// The minimum claude version that understands `--permission-mode auto`.
/// Older versions either silently fall back to `default` or reject the flag,
/// and the workspace pauses on every Bash prompt.
const CLAUDE_AUTO_MODE_MIN: (u32, u32, u32) = (2, 1, 83);

/// If the project wants auto-mode and the runner is claude, ensure the
/// workspace host's claude is new enough to understand it. Quiet pass-through
/// when the probe fails for unrelated reasons (claude missing from PATH,
/// weird output) — `wait_for_claude_ready` will surface a launch failure
/// downstream with a clearer signal than "version probe failed."
fn require_auto_mode_supported(
    host: &Host,
    runner: &shelbi_core::AgentRunnerSpec,
    mode: &str,
) -> Result<()> {
    if mode != "auto" {
        return Ok(());
    }
    // Only the `claude` CLI accepts `--permission-mode`; other runners
    // (codex etc.) reject the flag, so the version probe is meaningless
    // for them.
    if !shelbi_agent::RunnerAdapter::for_spec(runner).is_claude() {
        return Ok(());
    }
    let Some(version) = probe_claude_version(host) else {
        eprintln!(
            "shelbi: couldn't read `claude --version` on {host:?}; \
             skipping auto-mode compatibility check (claude {}+ required)",
            format_version(CLAUDE_AUTO_MODE_MIN),
        );
        return Ok(());
    };
    if version < CLAUDE_AUTO_MODE_MIN {
        return Err(Error::Other(format!(
            "claude {} on this workspace is too old for workspace_permissions_mode: auto \
             (need {}+, classifier-based auto-approval). Either upgrade claude on the \
             workspace host, or set `workspace_permissions_mode` in this project's config to \
             `acceptEdits` (auto-accept edits but still gate Bash) or `bypassPermissions` \
             (no seatbelt — auto-accept everything).",
            format_version(version),
            format_version(CLAUDE_AUTO_MODE_MIN),
        )));
    }
    Ok(())
}

/// Validate the runner shape and ensure the executable is visible from the
/// workspace host before we tear down the old pane. This turns a missing
/// `codex`/`claude` binary into an immediate dispatch error instead of a tmux
/// pane that starts, prints "command not found", and exits before Shelbi can
/// deliver the startup prompt.
fn require_runner_available(host: &Host, runner: &shelbi_core::AgentRunnerSpec) -> Result<()> {
    let command = runner.command.trim();
    if command.is_empty() {
        return Err(Error::Other(
            "invalid runner config: runner command is empty".into(),
        ));
    }
    if command.contains('\n') || command.contains('\0') {
        return Err(Error::Other(format!(
            "invalid runner config: runner command `{command:?}` contains an unsupported control character"
        )));
    }

    let script = if command.contains('/') {
        format!("test -x {}", shelbi_agent::shell_escape(command))
    } else {
        format!(
            "command -v -- {} >/dev/null",
            shelbi_agent::shell_escape(command)
        )
    };
    let out = match host {
        Host::Local => shelbi_ssh::run(host, ["sh", "-lc", &script]),
        Host::Ssh { .. } => crate::git::run_login_shell_script(host, &script),
    }
    .map_err(|e| Error::Other(format!("checking runner binary `{command}` failed: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "missing {} binary: runner command `{command}` was not found on the workspace host. \
             Install it there or update this workspace's runner config.",
            runner_label(command),
        )));
    }
    Ok(())
}

fn runner_label(command: &str) -> &'static str {
    match shelbi_agent::RunnerAdapter::for_command(command).kind() {
        shelbi_core::RunnerKind::Claude => "Claude",
        shelbi_core::RunnerKind::Codex => "Codex",
        shelbi_core::RunnerKind::Generic => "agent runner",
    }
}

/// Run `claude --version` on `host` and parse `(major, minor, patch)` from
/// its stdout. Returns `None` on any failure — caller decides how to react.
///
/// Local: shelbi's own PATH (inherited from the user's terminal) already
/// has claude. Remote: ssh's default non-login shell strips Homebrew /
/// nvm / asdf off PATH, so we re-exec through `$SHELL -lc` to source the
/// user's login rc — same trick we use to launch the agent itself.
fn probe_claude_version(host: &Host) -> Option<(u32, u32, u32)> {
    let out = match host {
        Host::Local => shelbi_ssh::run(host, ["claude", "--version"]).ok()?,
        // Remote: reuse the canonical login-shell bootstrap. Passing a bare
        // `$SHELL` / pre-quoted `'claude --version'` argv used to work only
        // because the SSH boundary did no escaping; now that it escapes
        // (Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/process-boundaries.md F2), route through `run_login_shell_script` so `$SHELL` still
        // expands and the raw script isn't double-quoted.
        Host::Ssh { .. } => crate::git::run_login_shell_script(host, "claude --version").ok()?,
    };
    if !out.status.success() {
        return None;
    }
    parse_claude_version(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `2.1.83 (Claude Code)` (or similar) into `(2, 1, 83)`.
fn parse_claude_version(s: &str) -> Option<(u32, u32, u32)> {
    let token = s.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn format_version((maj, min, pat): (u32, u32, u32)) -> String {
    format!("{maj}.{min}.{pat}")
}

/// Render the workspace `settings.json` for `project`, preferring the
/// dispatched agent's per-role `agents/<role>/settings.json` over the
/// project-wide `workspace-settings.json.template` when one is present.
///
/// Per-role precedence matters because the Phase 7 message-tail hooks
/// live in the role's settings file — a user editing
/// `agents/developer/settings.json` to add a project-specific hook
/// should see that change on the next dispatch, not silently lose it to
/// the project-wide fallback. Substitutes the legacy
/// `{{workspace_permissions_mode}}` placeholder for backwards-compat
/// with user-authored templates that still reference it.
///
/// Falls back to the project-wide template when `agent` is `None` (e.g.
/// embed tests) or when the role doesn't ship a settings.json (e.g.
/// a future codex-only role).
pub fn render_workspace_settings_preferring_agent(
    project: &shelbi_core::Project,
    agent: Option<&str>,
) -> Result<String> {
    let body = match agent {
        Some(name) => shelbi_state::load_agent_settings(&project.name, name)
            .map_err(|e| Error::Other(format!("{e}")))?,
        None => None,
    };
    let template = match body {
        Some(b) => b,
        None => shelbi_state::render_workspace_settings(project)
            .map_err(|e| Error::Other(format!("{e}")))?,
    };
    Ok(template.replace(
        "{{workspace_permissions_mode}}",
        &project.workspace_permissions_mode,
    ))
}

/// Write the rendered workspace `settings.json` to `<worktree>/.claude/` on
/// `host`. Local hosts get a direct filesystem write; remote hosts get an
/// `ssh mkdir -p` followed by `scp` of the rendered file. The workspace
/// machine never executes any shelbi code — this file is the whole
/// on-workspace footprint.
pub fn deploy_workspace_settings(
    host: &Host,
    worktree: &std::path::Path,
    rendered: &str,
) -> Result<()> {
    let claude_dir = worktree.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    match host {
        Host::Local => {
            std::fs::create_dir_all(&claude_dir).map_err(Error::Io)?;
            std::fs::write(&settings_path, rendered).map_err(Error::Io)?;
            Ok(())
        }
        Host::Ssh { host: ssh_host } => scp_settings_to_remote(
            ssh_host,
            &claude_dir.to_string_lossy(),
            &settings_path.to_string_lossy(),
            rendered,
        ),
    }
}

/// Relative path (from the worktree root) where the dispatched agent's
/// `instructions.md` is deployed. Kept in `.claude/` alongside the
/// settings + review marker so the whole shelbi deploy footprint is
/// gitignored together and there's exactly one place to look when
/// debugging an agent that loaded the wrong prompt.
pub const WORKTREE_AGENT_INSTRUCTIONS_REL: &str = ".claude/agent-instructions.md";
pub const WORKTREE_STARTUP_PROMPT_REL: &str = ".shelbi/startup-prompt.md";

/// Relative path (from the worktree root) where the dispatched agent's
/// `skills/` directory is mounted. Claude Code auto-loads any
/// `.claude/skills/` entries on launch.
pub const WORKTREE_AGENT_SKILLS_REL: &str = ".claude/skills";

const MESSAGE_TAIL_START_SH: &str = r#"#!/bin/sh
mkdir -p .shelbi/messages
if [ -z "${TASK_ID:-}" ]; then
  printf '%s no TASK_ID in env; message-tail not started (sidebar-click pane, or dispatch env leak)\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> .shelbi/messages/.no-task-id.log
  echo 'shelbi: SessionStart hook: TASK_ID unset; message channel disabled for this session' >&2
  exit 0
fi
LOCKDIR=.shelbi/messages/$TASK_ID.tail.d
if [ -f "$LOCKDIR/pid" ]; then
  kill "$(cat "$LOCKDIR/pid")" 2>/dev/null || true
  rm -rf "$LOCKDIR"
fi
mkdir -p "$LOCKDIR"
touch .shelbi/messages/$TASK_ID.log
tail -f -n 0 .shelbi/messages/$TASK_ID.log > .shelbi/messages/$TASK_ID.unread.log 2>/dev/null &
echo $! > "$LOCKDIR/pid"
"#;

const MESSAGE_DRAIN_STOP_SH: &str = r#"#!/bin/sh
[ -n "${TASK_ID:-}" ] || exit 0
UNREAD=.shelbi/messages/$TASK_ID.unread.log
PROC=$UNREAD.processing
if [ -s "$UNREAD" ]; then
  mv "$UNREAD" "$PROC"
  touch "$UNREAD"
  echo "<system-reminder>New orchestrator messages:"
  cat "$PROC"
  echo "</system-reminder>"
  HUB_ADDR="${SHELBI_HUB_ADDR:-${SHELBI_HUB_SOCK:+unix:$SHELBI_HUB_SOCK}}"
  if [ -n "$HUB_ADDR" ] && command -v jq >/dev/null 2>&1 && command -v nc >/dev/null 2>&1; then
    jq -r '.msg_id // empty' "$PROC" 2>/dev/null | while read MSG_ID; do
      [ -n "$MSG_ID" ] || continue
      ACK=$(printf '{"verb":"message-ack","project":"%s","task_id":"%s","msg_id":"%s"}\n' "$PROJECT" "$TASK_ID" "$MSG_ID")
      case "$HUB_ADDR" in
        tcp:*) HP=${HUB_ADDR#tcp:}; printf '%s' "$ACK" | nc "${HP%:*}" "${HP##*:}" 2>/dev/null || true ;;
        unix:*) printf '%s' "$ACK" | nc -U "${HUB_ADDR#unix:}" 2>/dev/null || true ;;
      esac
    done
  fi
  rm "$PROC"
fi
"#;

const PANE_IDLE_SH: &str = "#!/bin/sh\nprintf '\\033]2;shelbi:idle\\007'\n";
const PANE_WORKING_SH: &str = "#!/bin/sh\nprintf '\\033]2;shelbi:working\\007'\n";
const PANE_BLOCKED_SH: &str = "#!/bin/sh\nprintf '\\033]2;shelbi:blocked\\007'\n";

pub(crate) struct RunnerHookFile {
    pub(crate) rel_path: &'static str,
    pub(crate) body: &'static str,
    pub(crate) executable: bool,
}

pub(crate) const RUNNER_HOOK_FILES: &[RunnerHookFile] = &[
    RunnerHookFile {
        rel_path: ".shelbi/hooks/session-start.sh",
        body: MESSAGE_TAIL_START_SH,
        executable: true,
    },
    RunnerHookFile {
        rel_path: ".shelbi/hooks/stop.sh",
        body: MESSAGE_DRAIN_STOP_SH,
        executable: true,
    },
    RunnerHookFile {
        rel_path: ".shelbi/hooks/pane-idle.sh",
        body: PANE_IDLE_SH,
        executable: true,
    },
    RunnerHookFile {
        rel_path: ".shelbi/hooks/pane-working.sh",
        body: PANE_WORKING_SH,
        executable: true,
    },
    RunnerHookFile {
        rel_path: ".shelbi/hooks/pane-blocked.sh",
        body: PANE_BLOCKED_SH,
        executable: true,
    },
];

/// Deploy Shelbi-owned hook implementations under `<worktree>/.shelbi/hooks`.
///
/// The scripts are runner-neutral: one copy of each hook body
/// (`session-start.sh`, `stop.sh`, `pane-idle.sh`, ...) lives here and each
/// runner's config shim points at the shared path. Only Claude has a
/// Shelbi-wired hook channel today: its `.claude/settings.json` maps the
/// SessionStart/Stop/Notification/UserPromptSubmit/PreToolUse events onto these
/// scripts. Codex is not wired for hooks — the installed CLI rejects the
/// `-c core.hooksPath` override under strict validation and its only hook config
/// layers (`~/.codex/`, `<repo>/.codex/`) are user-owned, so Codex falls back to
/// prompt-level polling (see [`shelbi_agent::message_channel`]) and deploys no
/// hook config of its own. The bodies live in Shelbi's scratch namespace so
/// dispatches never overwrite user-owned `.codex/`/`.claude/` config and only
/// rewrite Shelbi-owned files. A workspace deployed by an older shelbi keeps its
/// stale `claude.*` filenames on disk; nothing regenerates them, and the next
/// launch re-renders settings.json to reference the neutral scripts, so a live
/// workspace converges on relaunch without manual cleanup.
pub fn deploy_runner_hooks(host: &Host, worktree: &Path) -> Result<()> {
    for file in RUNNER_HOOK_FILES {
        deploy_runner_hook_file(host, worktree, file)?;
    }
    Ok(())
}

fn deploy_runner_hook_file(host: &Host, worktree: &Path, file: &RunnerHookFile) -> Result<()> {
    let dest = worktree.join(file.rel_path);
    let dest_dir = dest
        .parent()
        .ok_or_else(|| Error::Other(format!("invalid runner hook path `{}`", file.rel_path)))?;
    match host {
        Host::Local => {
            std::fs::create_dir_all(dest_dir).map_err(Error::Io)?;
            std::fs::write(&dest, file.body).map_err(Error::Io)?;
            if file.executable {
                set_executable(&dest)?;
            }
            Ok(())
        }
        Host::Ssh { host: ssh_host } => {
            scp_text_to_remote(
                ssh_host,
                &dest_dir.to_string_lossy(),
                &dest.to_string_lossy(),
                file.body,
                "runner-hook",
            )?;
            if file.executable {
                let chmod = shelbi_ssh::run(host, ["chmod", "755", &dest.to_string_lossy()])
                    .map_err(Error::Io)?;
                if !chmod.status.success() {
                    return Err(Error::Command {
                        cmd: format!("chmod 755 {}", dest.display()),
                        status: chmod.status.to_string(),
                        stderr: String::from_utf8_lossy(&chmod.stderr).into_owned(),
                    });
                }
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path).map_err(Error::Io)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(Error::Io)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Deploy the dispatched agent's `instructions.md` to the worktree and
/// refresh `.claude/skills/` from the agent's `skills/` directory. One
/// orchestration helper so the spawn path doesn't have to manage the
/// individual primitives; the caller just hands us a (host, worktree,
/// project, agent) tuple and we do the rest.
///
/// Idempotent and overwrite-safe: every call rewrites the instructions
/// file and clears `.claude/skills/` before mounting, so the worktree's
/// agent context reflects the *current* agent — not a leftover from a
/// prior task on the same workspace.
pub fn deploy_agent_context(
    host: &Host,
    worktree: &Path,
    project_name: &str,
    agent: &str,
) -> Result<()> {
    // Compose preamble (`agents/_shared/preamble.md`, if present) + the
    // agent's `instructions.md` into one body. `compose_agent_prompt`
    // handles the missing-preamble case (just the agent's prompt). Any
    // file-read failure surfaces from there with the same shape the old
    // direct-read used.
    let mut composed = shelbi_state::compose_agent_prompt(project_name, agent)
        .map_err(|e| Error::Other(format!("{e}")))?;
    // Orchestrator-only: if a `handoff.md` was left by the previous
    // instance (on `shelbi reload` or `shelbi quit`), splice it onto
    // the end of the system prompt as a `<system-reminder>` block so
    // the new instance picks up where the old one left off. Read-and-
    // delete — handoff is one-shot; persistent state lives in
    // `state.json`. Skipped silently for every non-orchestrator agent
    // (developer dispatches, custom agents) so a worktree's deploy
    // never ingests handoff data meant for the dashboard.
    if agent == shelbi_state::ORCHESTRATOR_AGENT {
        match shelbi_state::take_orchestrator_handoff(project_name) {
            Ok(Some(handoff)) => {
                composed = splice_orchestrator_handoff(&composed, &handoff);
            }
            Ok(None) => {}
            Err(e) => {
                // Surface but don't fail the deploy — a missing handoff
                // is a degraded start (new instance is cold), not a
                // broken one.
                tracing::warn!(
                    project = project_name,
                    error = %e,
                    "take_orchestrator_handoff failed; starting orchestrator cold",
                );
            }
        }
    }
    deploy_agent_instructions(host, worktree, &composed)?;

    let skills_src = shelbi_state::agent_skills_dir(project_name, agent)?;
    refresh_agent_skills(host, worktree, &skills_src)?;

    // Best-effort: nudge the user toward the new agents/ layout the
    // first time a dispatch lands while a legacy CLAUDE.md is still on
    // disk. Idempotent across multiple dispatches per orchestrator
    // session (state-flag gated). Don't fail the dispatch on a hint
    // write error — the user can still hand-migrate.
    let _ = shelbi_state::maybe_emit_claude_md_migration_hint(project_name);
    Ok(())
}

/// Wrap `handoff` in a `<system-reminder>` block and append it to
/// `composed`. The block is rendered as plain text — the orchestrator's
/// claude runtime renders it as a system-reminder in the conversation
/// transcript, which is exactly how we want the next instance to read
/// it (load-bearing context from the previous instance, distinct from
/// the evergreen instructions above it).
///
/// Pure on its inputs so it's unit-testable without a real handoff
/// file on disk.
fn splice_orchestrator_handoff(composed: &str, handoff: &str) -> String {
    let mut out = String::with_capacity(composed.len() + handoff.len() + 64);
    out.push_str(composed);
    if !composed.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("<system-reminder>\n");
    out.push_str(handoff);
    if !handoff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</system-reminder>\n");
    out
}

/// Write `instructions` to `<worktree>/.claude/agent-instructions.md` so
/// the runner can `--append-system-prompt "$(cat …)"` from it on launch.
/// Mirrors [`deploy_workspace_settings`]'s local-vs-remote split.
pub fn deploy_agent_instructions(host: &Host, worktree: &Path, instructions: &str) -> Result<()> {
    let claude_dir = worktree.join(".claude");
    let dest_path = claude_dir.join("agent-instructions.md");
    match host {
        Host::Local => {
            std::fs::create_dir_all(&claude_dir).map_err(Error::Io)?;
            std::fs::write(&dest_path, instructions).map_err(Error::Io)?;
            Ok(())
        }
        Host::Ssh { host: ssh_host } => scp_text_to_remote(
            ssh_host,
            &claude_dir.to_string_lossy(),
            &dest_path.to_string_lossy(),
            instructions,
            "agent-instructions",
        ),
    }
}

fn render_startup_prompt(
    prompt: &str,
    include_agent_instructions: bool,
    runner: &shelbi_core::AgentRunnerSpec,
) -> String {
    let mut out = String::new();
    if include_agent_instructions && !shelbi_agent::RunnerAdapter::for_spec(runner).is_claude() {
        out.push_str("Read `.claude/agent-instructions.md` first. ");
        out.push_str(
            "Treat it as your developer-agent instructions for this Shelbi workspace.\n\n",
        );
    }
    out.push_str(prompt);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn deploy_startup_prompt(host: &Host, worktree: &Path, prompt: &str) -> Result<()> {
    let dest_path = worktree.join(WORKTREE_STARTUP_PROMPT_REL);
    let dest_dir = dest_path.parent().ok_or_else(|| {
        Error::Other(format!(
            "invalid startup prompt path `{WORKTREE_STARTUP_PROMPT_REL}`"
        ))
    })?;
    match host {
        Host::Local => {
            std::fs::create_dir_all(dest_dir)
                .map_err(|e| Error::Other(format!("startup prompt delivery failure: {e}")))?;
            std::fs::write(&dest_path, prompt)
                .map_err(|e| Error::Other(format!("startup prompt delivery failure: {e}")))?;
            Ok(())
        }
        Host::Ssh { host: ssh_host } => scp_text_to_remote(
            ssh_host,
            &dest_dir.to_string_lossy(),
            &dest_path.to_string_lossy(),
            prompt,
            "startup-prompt",
        )
        .map_err(|e| Error::Other(format!("startup prompt delivery failure: {e}"))),
    }
}

/// Clear `<worktree>/.claude/skills/` and recursively mirror
/// `skills_src` into it. A non-existent or empty `skills_src` just
/// produces an empty destination — the v1 default agents ship with no
/// skills, so this is the normal happy path.
pub fn refresh_agent_skills(host: &Host, worktree: &Path, skills_src: &Path) -> Result<()> {
    let dest = worktree.join(".claude").join("skills");
    let dest_str = dest.to_string_lossy().into_owned();

    // Clear first. `rm -rf` is idempotent — succeeds whether the path
    // exists or not, which keeps the first-dispatch (nothing there yet)
    // and Nth-dispatch (carry-over from a different agent) cases on the
    // same code path.
    let rm = shelbi_ssh::run(host, ["rm", "-rf", &dest_str]).map_err(Error::Io)?;
    if !rm.status.success() {
        return Err(Error::Command {
            cmd: format!("rm -rf {dest_str}"),
            status: rm.status.to_string(),
            stderr: String::from_utf8_lossy(&rm.stderr).into_owned(),
        });
    }

    // Always (re)create the destination — even when skills_src is empty
    // we want `.claude/skills/` to exist so the runner's skill loader
    // doesn't trip on a missing path.
    let mkdir = shelbi_ssh::run(host, ["mkdir", "-p", &dest_str]).map_err(Error::Io)?;
    if !mkdir.status.success() {
        return Err(Error::Command {
            cmd: format!("mkdir -p {dest_str}"),
            status: mkdir.status.to_string(),
            stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
        });
    }

    if !skills_src.is_dir() {
        // Agent's skills/ dir not on disk (legacy materialize, user nuked
        // it, etc.). The hub side's self-heal will recreate it on next
        // `shelbi reload`; for now an empty deploy is correct.
        return Ok(());
    }

    let entries: Vec<_> = std::fs::read_dir(skills_src)
        .map_err(Error::Io)?
        .collect::<std::result::Result<_, _>>()
        .map_err(Error::Io)?;
    if entries.is_empty() {
        return Ok(());
    }

    match host {
        Host::Local => copy_dir_contents_local(skills_src, &dest)?,
        Host::Ssh { host: ssh_host } => copy_dir_contents_to_remote(ssh_host, skills_src, &dest)?,
    }
    Ok(())
}

/// Recursively copy the contents of `src` (a directory) into `dest`. The
/// destination must already exist. Used to mirror the agent's `skills/`
/// directory into the worktree on local hosts; remote hosts go through
/// [`copy_dir_contents_to_remote`].
fn copy_dir_contents_local(src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let entry_path = entry.path();
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(Error::Io)?;
            copy_dir_contents_local(&entry_path, &target)?;
        } else {
            std::fs::copy(&entry_path, &target).map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Recursively copy `src` (a directory's contents) into a remote `dest`
/// over SCP. v1 default agents ship with empty skills, so this happy
/// path is normally a no-op; when users start populating `skills/` the
/// tree is expected to be small (a handful of `.md`s + maybe an
/// `assets/` dir). Per-file SCP is plenty.
fn copy_dir_contents_to_remote(ssh_host: &str, src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let entry_path = entry.path();
        let target = dest.join(entry.file_name());
        let target_str = target.to_string_lossy().into_owned();
        if file_type.is_dir() {
            let mkdir = shelbi_ssh::run(
                &Host::Ssh {
                    host: ssh_host.to_string(),
                },
                ["mkdir", "-p", &target_str],
            )
            .map_err(Error::Io)?;
            if !mkdir.status.success() {
                return Err(Error::Command {
                    cmd: format!("ssh {ssh_host} mkdir -p {target_str}"),
                    status: mkdir.status.to_string(),
                    stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
                });
            }
            copy_dir_contents_to_remote(ssh_host, &entry_path, &target)?;
        } else {
            let dest_uri = scp_remote_target(ssh_host, &target_str);
            let mut cmd = std::process::Command::new("scp");
            cmd.arg("-q").arg("-B").arg(&entry_path).arg(&dest_uri);
            let out = cmd.output().map_err(Error::Io)?;
            if !out.status.success() {
                return Err(Error::Command {
                    cmd: format!("scp {} {dest_uri}", entry_path.display()),
                    status: out.status.to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Build the `cd <wt> && … exec $SHELL -lc <launch>` line we send into
/// the remote tmux pane, prefixing the exec with `SHELBI_HUB_SOCK=<path>`
/// so the agent picks up the SSH-reverse-forwarded hub socket
/// ([`shelbi_state::remote_hub_socket_path`], default
/// `/tmp/shelbi-hub-<uid>.sock`). Worker→hub events (`pane_alive=false`,
/// future verbs) flow through that socket; with no `SHELBI_HUB_SOCK` set
/// the agent's instructions paragraph falls through to a no-op and loss
/// is accepted (the spec calls this "best-effort + hub-side detection").
///
/// Inputs to [`local_pane_tmux_argv`] — mirrors the local dispatch
/// path's tmux invocation exactly so tests can assert on the argv shape
/// without spinning up a tmux server.
struct LocalPaneTmuxArgs<'a> {
    /// `true` → `tmux new-session -d -s <session> -n <window> …`.
    /// `false` → `tmux new-window -d -t =<session>: -n <window> …` inside
    /// the already-live project session.
    create_new_session: bool,
    session: &'a str,
    window: &'a str,
    task_id: &'a str,
    project: &'a str,
    /// The workspace name, injected as `SHELBI_WORKSPACE` for a review
    /// workspace so the Review agent's `shelbi workspace serve` resolves its
    /// own slot without guessing.
    workspace: &'a str,
    hub_sock: &'a str,
    /// Deterministic dev-server port for a review workspace, injected as
    /// `PORT` into the pane env. `None` on the dev path (no `PORT`).
    port: Option<u16>,
    pane_cmd: &'a str,
}

/// Build the tmux argv for the local dispatch path. Injects
/// `TASK_ID` / `PROJECT` / `SHELBI_HUB_SOCK` (and `PORT` for a review
/// workspace) via tmux `-e` so the pane
/// wrapper inherits them regardless of when the caller's state save
/// lands — the caller writes `assigned_to` / `column=in_progress`
/// AFTER `start_workspace_on_task` returns, so a state lookup at
/// wrapper startup would come up empty and the Phase 7 message-tail
/// hooks would silently no-op (the exact bug the outer function is
/// wired to prevent). See `open/pane.rs` where the wrapper prefers
/// inherited env over the state lookup.
fn local_pane_tmux_argv(a: LocalPaneTmuxArgs<'_>) -> Vec<String> {
    let task_env = format!("TASK_ID={}", a.task_id);
    let project_env = format!("PROJECT={}", a.project);
    let hub_env = format!("SHELBI_HUB_SOCK={}", a.hub_sock);
    let mut argv: Vec<String> = if a.create_new_session {
        vec![
            "tmux".into(),
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            a.session.into(),
            "-n".into(),
            a.window.into(),
        ]
    } else {
        vec![
            "tmux".into(),
            "new-window".into(),
            "-d".into(),
            "-t".into(),
            format!("{}:", shelbi_tmux::session_target(a.session)),
            "-n".into(),
            a.window.into(),
        ]
    };
    argv.push("-e".into());
    argv.push(task_env);
    argv.push("-e".into());
    argv.push(project_env);
    argv.push("-e".into());
    argv.push(hub_env);
    // Review workspaces pin a deterministic dev-server PORT so the review
    // agent binds a slot that won't collide with a concurrent review
    // workspace, plus SHELBI_WORKSPACE so `shelbi workspace serve` resolves
    // this slot. Dev workspaces pass `None` and get neither.
    if let Some(port) = a.port {
        argv.push("-e".into());
        argv.push(format!("PORT={port}"));
        argv.push("-e".into());
        argv.push(format!("SHELBI_WORKSPACE={}", a.workspace));
    }
    // The pane command runs through `sh -c` so tmux picks up the user's
    // PATH from the tmux server's existing env (Homebrew, asdf, etc).
    argv.push("sh".into());
    argv.push("-c".into());
    argv.push(a.pane_cmd.into());
    argv
}

/// We park the assignment immediately before `exec` so it scopes to the
/// agent process (the surrounding `$SHELL -lc` strips its own
/// environment otherwise — env-prefix-before-exec is the POSIX idiom
/// for "set var for THIS command only and inherit it"). `port`, when
/// `Some`, is injected as `PORT` in the same env prefix (review
/// workspaces); dev workspaces pass `None`.
///
/// `host` selects how the worker reaches the hub. The pane always gets
/// `SHELBI_HUB_ADDR` (scheme-tagged: `unix:<path>` or `tcp:<addr>:<port>`) so
/// the send helper can dispatch either transport; on a Unix-forward host we
/// *also* set the legacy `SHELBI_HUB_SOCK` so the plain `nc -U "$SHELBI_HUB_SOCK"`
/// path keeps working unchanged. A TCP-fallback host (Tailscale SSH) gets only
/// `SHELBI_HUB_ADDR`, since there is no usable Unix landing socket there.
fn remote_cd_launch(
    host: &Host,
    worktree: &Path,
    launch: &str,
    port: Option<u16>,
    workspace: &str,
) -> String {
    let hub_env = remote_hub_env_prefix(host);
    // Review workspaces (Some port) also get SHELBI_WORKSPACE so the Review
    // agent's `shelbi workspace serve` resolves its slot.
    let port_env = match port {
        Some(p) => format!(
            "PORT={p} SHELBI_WORKSPACE={ws} ",
            ws = shelbi_agent::shell_escape(workspace)
        ),
        None => String::new(),
    };
    format!(
        "cd {wd} && LANG=C.UTF-8 {port_env}{hub_env}exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
        wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        launch = shelbi_agent::shell_escape(launch),
    )
}

/// Build the `SHELBI_HUB_ADDR=… [SHELBI_HUB_SOCK=…] ` env prefix (trailing
/// space included) a remote pane needs to reach the hub daemon, resolving the
/// per-host forward decision. Shared by [`remote_cd_launch`] and the
/// `shelbi spawn` path so the two can't drift.
pub fn remote_hub_env_prefix(host: &Host) -> String {
    let hostname = match host {
        Host::Local => return String::new(),
        Host::Ssh { host } => host.as_str(),
    };
    match shelbi_state::remote_hub_endpoint(hostname) {
        endpoint @ shelbi_state::HubEndpoint::Unix(_) => {
            // Unix forward: set both the scheme-tagged addr and the legacy
            // SHELBI_HUB_SOCK path the existing worker snippets consume.
            let sock = match &endpoint {
                shelbi_state::HubEndpoint::Unix(p) => p.to_string_lossy().into_owned(),
                _ => unreachable!(),
            };
            format!(
                "SHELBI_HUB_ADDR={addr} SHELBI_HUB_SOCK={sock} ",
                addr = shelbi_agent::shell_escape(&endpoint.addr_env_value()),
                sock = shelbi_agent::shell_escape(&sock),
            )
        }
        endpoint @ shelbi_state::HubEndpoint::Tcp { .. } => {
            // TCP loopback: no Unix socket exists, so only the addr is set.
            format!(
                "SHELBI_HUB_ADDR={addr} ",
                addr = shelbi_agent::shell_escape(&endpoint.addr_env_value()),
            )
        }
    }
}

/// Construct the shell command that launches the agent runner for a workspace
/// pane — the single launch-command builder shared by BOTH host paths so the
/// local and remote dispatch flows can't drift:
///
/// - **Local** panes run the `shelbi open <ws> --as-pane` lifecycle wrapper,
///   which calls this to build the command it execs (see the wrapper in
///   `shelbi-cli`'s `open/pane.rs`).
/// - **Remote** panes have no shelbi binary to run the wrapper, so
///   [`deploy_and_spawn`] calls this directly and sends the result into the
///   pane via [`remote_cd_launch`].
///
/// `--permission-mode <mode>` is injected onto the runner's command line (via
/// [`shelbi_agent::with_permission_mode`]) rather than trusted to the rendered
/// `.claude/settings.json`: settings-based mode is fragile (silent fallback to
/// interactive on any I/O race or version regression), so the CLI flag is
/// authoritative and belongs to the spawn path where we already know the
/// project's mode. When `include_agent_instructions` is set and the runner is
/// `claude`, the deployed `instructions.md` is wired in as
/// `--append-system-prompt "$(cat …)"` (see [`with_agent_system_prompt`]).
pub fn workspace_launch_command(
    runner: &shelbi_core::AgentRunnerSpec,
    permissions_mode: &str,
    include_agent_instructions: bool,
    resume: bool,
) -> String {
    workspace_launch_command_with_startup_prompt(
        runner,
        permissions_mode,
        include_agent_instructions,
        resume,
        None,
    )
}

pub fn workspace_launch_command_with_startup_prompt(
    runner: &shelbi_core::AgentRunnerSpec,
    permissions_mode: &str,
    include_agent_instructions: bool,
    resume: bool,
    startup_prompt_rel: Option<&str>,
) -> String {
    // `resume` (a `shelbi task resume`) adds `--continue` for a claude runner
    // so the pane reloads its prior conversation instead of starting cold —
    // see [`shelbi_agent::with_continue`]. It's a no-op for a normal dispatch
    // and for non-claude runners.
    let runner_with_mode = shelbi_agent::with_permission_mode(runner, permissions_mode);
    let runner_resolved = shelbi_agent::with_continue(&runner_with_mode, resume);
    let launch = with_agent_system_prompt(
        &shelbi_agent::launch_command(&runner_resolved),
        include_agent_instructions.then_some(WORKTREE_AGENT_INSTRUCTIONS_REL),
        runner,
    );
    with_startup_prompt(&launch, startup_prompt_rel, runner, resume)
}

/// Append the `--append-system-prompt "$(cat .claude/agent-instructions.md)"`
/// flag to the runner's launch command when an agent is being dispatched
/// AND the runner is `claude` (the only CLI that understands the flag).
/// Returns `launch` unchanged for non-claude runners or when no agent
/// instructions are being deployed.
///
/// The `$(cat …)` substitution is intentional: keeping the prompt body
/// out of the command line means the launched line stays human-readable
/// in the pane (no 10 KB of `# Task …\n\n` scrollback noise) and avoids
/// the per-platform ARG_MAX risk of inlining a large prompt.
fn with_agent_system_prompt(
    launch: &str,
    instructions_rel: Option<&str>,
    runner: &shelbi_core::AgentRunnerSpec,
) -> String {
    let Some(rel) = instructions_rel else {
        return launch.to_string();
    };
    if !shelbi_agent::RunnerAdapter::for_spec(runner).is_claude() {
        // Other runners (codex etc.) don't understand the flag; leave
        // them alone. The agent's instructions.md is still deployed to
        // the worktree for callers that want to read it manually.
        return launch.to_string();
    }
    format!(
        "{launch} --append-system-prompt \"$(cat {rel})\"",
        rel = shelbi_agent::shell_escape(rel),
    )
}

fn effective_prompt_injection_for_spawn(
    runner: &shelbi_core::AgentRunnerSpec,
    resume: bool,
) -> shelbi_core::PromptInjectionSpec {
    if resume && shelbi_agent::RunnerAdapter::for_spec(runner).is_claude() {
        return shelbi_core::PromptInjectionSpec::paste();
    }
    runner.effective_prompt_injection()
}

fn with_startup_prompt(
    launch: &str,
    startup_prompt_rel: Option<&str>,
    runner: &shelbi_core::AgentRunnerSpec,
    resume: bool,
) -> String {
    let Some(rel) = startup_prompt_rel else {
        return launch.to_string();
    };
    match effective_prompt_injection_for_spawn(runner, resume).kind {
        PromptInjectionKind::PositionalArg => format!(
            "{launch} \"$(cat {rel})\"",
            rel = shelbi_agent::shell_escape(rel),
        ),
        PromptInjectionKind::FlagFile => {
            let spec = effective_prompt_injection_for_spawn(runner, resume);
            let flag = spec.flag.as_deref().unwrap_or("--message-file");
            format!(
                "{launch} {flag} {rel}",
                flag = shelbi_agent::shell_escape(flag),
                rel = shelbi_agent::shell_escape(rel),
            )
        }
        PromptInjectionKind::Stdin => format!(
            "cat {rel} | {launch}",
            rel = shelbi_agent::shell_escape(rel),
        ),
        PromptInjectionKind::Paste => launch.to_string(),
    }
}

/// Build the `host:path` target scp expects, shell-escaping the *path*
/// half. scp splits its target on the first `:` and hands everything after
/// it to the remote login shell, which re-tokenizes it — so a spaced or
/// metacharacter path word-splits there exactly like an unescaped SSH argv
/// would. This is the one remote path shelbi drives that doesn't ride
/// through `shelbi_ssh` (and its per-arg escaping), so it escapes here.
fn scp_remote_target(ssh_host: &str, remote_path: &str) -> String {
    format!("{ssh_host}:{}", shelbi_agent::shell_escape(remote_path))
}

fn scp_settings_to_remote(
    ssh_host: &str,
    remote_dir: &str,
    remote_path: &str,
    rendered: &str,
) -> Result<()> {
    scp_text_to_remote(
        ssh_host,
        remote_dir,
        remote_path,
        rendered,
        "workspace-settings",
    )
}

/// Push `body` to `remote_path` on `ssh_host`, ensuring `remote_dir`
/// exists first. Shared by the workspace-settings and agent-instructions
/// deploy paths so both go through the same scp + mkdir routine. `tag`
/// is folded into the local tempfile name so a crash mid-deploy leaves
/// debuggable breadcrumbs.
fn scp_text_to_remote(
    ssh_host: &str,
    remote_dir: &str,
    remote_path: &str,
    body: &str,
    tag: &str,
) -> Result<()> {
    // 1. Ensure the .claude/ dir exists on the remote.
    let mkdir = shelbi_ssh::run(
        &Host::Ssh {
            host: ssh_host.to_string(),
        },
        ["mkdir", "-p", remote_dir],
    )
    .map_err(Error::Io)?;
    if !mkdir.status.success() {
        return Err(Error::Command {
            cmd: format!("ssh {ssh_host} mkdir -p {remote_dir}"),
            status: mkdir.status.to_string(),
            stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
        });
    }

    // 2. Stage the body in a local tempfile, then scp it. The tempfile
    //    is in $TMPDIR so the local FS handles cleanup if we crash
    //    before unlinking it.
    let tmp_path = std::env::temp_dir().join(format!(
        "shelbi-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&tmp_path, body).map_err(Error::Io)?;

    // The local `tmp_path` is a real argv element to the local scp binary and
    // needs no escaping; the remote `remote_path` half does — see
    // [`scp_remote_target`].
    let dest = scp_remote_target(ssh_host, remote_path);
    let mut cmd = std::process::Command::new("scp");
    // -q quiets scp's progress chatter; -B disables interactive prompts
    // (we expect keys via ssh-agent).
    cmd.arg("-q").arg("-B").arg(&tmp_path).arg(&dest);
    let out = cmd.output().map_err(Error::Io)?;
    let _ = std::fs::remove_file(&tmp_path);
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("scp {} {dest}", tmp_path.display()),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Build the initial prompt: the task body + the loop-closing instructions
/// that tell the workspace how to rebase onto current `default_branch` and then
/// mark itself done.
///
/// The handoff is a file marker, not a pane title or a `shelbi` CLI call.
/// The workspace writes its task id into `<worktree>/.claude/shelbi-ready`
/// (see [`workspace_ready_marker`]); the hub poller picks it up and moves the
/// task to the review column. This survives Claude's own OSC pane-title
/// writes and the Stop hook, both of which used to clobber a `shelbi:review`
/// title before the poller could read it, and it needs no `shelbi` binary on
/// the workspace host.
///
/// The rebase step lives in the prompt (not in poll-side promotion code) so
/// the workspace re-runs its checks against the rebased base before signalling
/// review — a hub-side rebase happens after handoff, when there's no agent
/// around to fix conflicts or re-run tests.
fn compose_prompt(
    task_id: &str,
    branch: &str,
    body: &str,
    marker: &Path,
    default_branch: &str,
    project: &str,
    polls_messages: bool,
) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}\n")
    } else {
        trimmed.to_string()
    };
    let id_esc = shelbi_agent::shell_escape(task_id);
    let marker_esc = shelbi_agent::shell_escape(&marker.to_string_lossy());
    // Write to a sibling temp file and `mv` it into place so the poller
    // never `cat`s a half-written marker (a torn body would fail
    // `validate_task_id` in `read_ready_marker` and stall the handoff).
    // `mv` within one directory is an atomic rename.
    let marker_tmp = {
        let mut s = marker.as_os_str().to_owned();
        s.push(".tmp");
        PathBuf::from(s)
    };
    let marker_tmp_esc = shelbi_agent::shell_escape(&marker_tmp.to_string_lossy());
    let polling_section = if polls_messages {
        message_polling_section(task_id, project, &id_esc)
    } else {
        String::new()
    };
    format!(
        "{body_section}\n\n\
         ---\n\
         You are working on task `{task_id}` on branch `{branch}`. When \
         the work is complete and committed, do these two things in order:\n\
         \n\
         1. Rebase your branch onto current `{default_branch}` so the review \
         sees a clean diff against an up-to-date base — a stale base produces \
         test failures that have nothing to do with your change and inflates \
         the diff with commits already on `{default_branch}`:\n\
         \n\
         git fetch origin {default_branch} && git rebase origin/{default_branch}\n\
         \n\
         If the rebase produces conflicts, resolve them, run `git rebase \
         --continue`, and re-run any affected tests before moving on. Do NOT \
         write the marker until the rebase is complete and your work still \
         passes against the rebased base.\n\
         \n\
         2. Signal that it's ready for handoff by writing the task id to the \
         ready marker file:\n\
         \n\
         printf '%s\\n' {id_esc} > {marker_tmp_esc} && mv {marker_tmp_esc} {marker_esc}\n\
         \n\
         The hub watches for this file and moves your task to the review \
         column on its next poll. Write the marker once; you can keep \
         working in this pane and talk to the user afterward without \
         affecting the handoff.{polling_section}"
    )
}

/// Build the prompt sent into a **resumed** pane (`shelbi task resume`). It's
/// [`compose_prompt`]'s output — the task body plus the identical rebase +
/// review-marker handoff — with a short resume banner prepended so the worker
/// knows it's being picked back up rather than freshly dispatched.
///
/// The banner's wording depends on `conversation_resumed`:
///
/// - `true` (a claude runner launched with `--continue`): the pane already
///   holds the prior conversation, so the banner just tells the worker it was
///   resumed and to continue where it left off. Its own committed + uncommitted
///   work is still in the worktree.
/// - `false` (any other runner, launched cold): the conversation is gone, so
///   the banner points the worker at the work already in its worktree —
///   commits and uncommitted changes preserved — and tells it to read that to
///   re-establish context before continuing.
///
/// Reusing `compose_prompt` for the body + handoff keeps the resume path from
/// drifting from the dev-start path on the load-bearing bits (the atomic marker
/// write, the rebase-before-marker ordering, the message-polling section for
/// runners Shelbi cannot wire with hooks).
#[allow(clippy::too_many_arguments)]
fn compose_resume_prompt(
    task_id: &str,
    branch: &str,
    body: &str,
    marker: &Path,
    default_branch: &str,
    project: &str,
    polls_messages: bool,
    conversation_resumed: bool,
) -> String {
    let banner = if conversation_resumed {
        format!(
            "**Resumed.** You are being resumed on task `{task_id}` (branch \
             `{branch}`) — the conversation above is your own prior work on it. \
             Your commits and any uncommitted changes are still in this worktree, \
             exactly where you left them. Pick up where you left off and finish \
             the task; the original instructions follow for reference.\n\n"
        )
    } else {
        format!(
            "**Resumed.** You are being resumed on task `{task_id}` (branch \
             `{branch}`). Your earlier work on this task — commits and any \
             uncommitted changes — is preserved in this worktree. Start by \
             reviewing what's already there (`git log`, `git status`, `git diff`) \
             to re-establish context, then continue and finish the task. The \
             original instructions follow.\n\n"
        )
    };
    let base = compose_prompt(
        task_id,
        branch,
        body,
        marker,
        default_branch,
        project,
        polls_messages,
    );
    format!("{banner}{base}")
}

/// The pull-style message-delivery paragraph appended to the prompt for
/// runners without a hook surface (aider, custom CLIs, …). Claude and Codex
/// receive hub messages through hooks and never see this section.
///
/// Codex drives every step — file reads, edits, and commands — through its
/// `shell` tool, so "after every shell command" is the concrete, guaranteed
/// cadence the task plan asks for: any non-trivial work runs at least one
/// shell command in a short window, where "between significant steps" would
/// be vague enough to skip.
///
/// The cursor file holds the 1-indexed number of the *next* unread line. It
/// starts at 1 (read from the top), and after each poll advances to
/// `<line count> + 1` so the following `tail -n +$CURSOR` begins past the
/// last line already read — re-reading is what an off-by-one here would
/// cause, so the `+ 1` is load-bearing. The `2>/dev/null` guards keep a
/// cold start (no log yet) from erroring before the hub's first write.
fn message_polling_section(task_id: &str, project: &str, id_esc: &str) -> String {
    format!(
        "\n\n\
         ---\n\
         Your message log is at `.shelbi/messages/{task_id}.log` (relative to \
         this worktree). The orchestrator appends directives there while you \
         work, and you must pull them yourself. **After every shell command \
         you run**, check the log for new lines:\n\
         \n\
         CURSOR=$(cat .shelbi/messages/{id_esc}.cursor 2>/dev/null || echo 1)\n\
         tail -n +\"$CURSOR\" .shelbi/messages/{id_esc}.log 2>/dev/null\n\
         echo $(($(wc -l < .shelbi/messages/{id_esc}.log 2>/dev/null || echo 0) + 1)) > .shelbi/messages/{id_esc}.cursor\n\
         \n\
         Act on any new messages before continuing your current work. Each \
         line is one JSON message with a `msg_id`; for every new message, ack \
         it so the orchestrator knows it landed:\n\
         \n\
         echo '{{\"verb\":\"message-ack\",\"project\":\"{project}\",\"task_id\":\"{task_id}\",\"msg_id\":\"<msg-id>\"}}' | nc -U \"$SHELBI_HUB_SOCK\""
    )
}

/// Resolve the ref a brand-new task branch should be cut from.
///
/// The machine's local `default_branch` ref can be arbitrarily stale: on a
/// remote workspace clone nothing advances it between dispatches (merges
/// land on GitHub via `gh pr merge`, and no fetch ran here), so cutting from
/// it silently bases the task on day-old code — reviews analyze superseded
/// sources, fixes edit files reworked upstream, and `zen probe` diffs
/// explode because the branch root predates the current default. See
/// task `dispatch-stale-base-branch-cut` for the observed fallout.
///
/// So: when the repo has an `origin` remote, fetch the default branch and
/// cut from the freshly-updated remote-tracking ref (`origin/<default>`).
/// A repo with no `origin` (local-only project, test fixture) falls back to
/// the local ref — with no remote there is no fresher truth to fetch.
///
/// A failing fetch when `origin` exists is a HARD error, not a silent
/// fallback — same principle as the `fetch_probe_base` silent-fallback
/// finding (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/orchestrator-zen.md F7): the caller must abort the dispatch
/// (task stays put, event emitted) rather than cut from a possibly-stale
/// ref.
fn resolve_fresh_cut_base(host: &Host, repo: &str, default_branch: &str) -> Result<String> {
    let has_origin = shelbi_ssh::run(
        host,
        ["git", "-C", repo, "config", "--get", "remote.origin.url"],
    )
    .map_err(Error::Io)?
    .status
    .success();
    if !has_origin {
        return Ok(default_branch.to_string());
    }
    let fetch = shelbi_ssh::run(host, ["git", "-C", repo, "fetch", "origin", default_branch])
        .map_err(Error::Io)?;
    if !fetch.status.success() {
        return Err(Error::Other(format!(
            "refusing to cut a task branch from a possibly-stale `{default_branch}`: \
             `git fetch origin {default_branch}` failed in {repo}: {}",
            String::from_utf8_lossy(&fetch.stderr).trim(),
        )));
    }
    Ok(format!("origin/{default_branch}"))
}

/// Ensure the worktree exists and is checked out on `branch`. Creates the
/// worktree off the project's default branch if absent, creates the branch
/// off the default if it doesn't exist yet (fetching `origin/<default>`
/// first so the cut base is current — see [`resolve_fresh_cut_base`]), and
/// bails if the worktree has uncommitted changes (otherwise switching
/// branches would lose work).
fn sync_worktree(
    project: &Project,
    host: &Host,
    machine: &Machine,
    worktree: &std::path::Path,
    branch: &str,
    default_branch: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();

    // Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F5:
    // a dispatch killed after the worktree *dir* was created but before
    // its `.git` gitlink was written leaves `<wt>` present-but-invalid.
    // Every later `git worktree add <wt>` then aborts with "already exists"
    // and wedges the workspace forever. Prune stale bookkeeping first, then
    // — if the dir is present without a valid `.git` — force-remove any
    // lingering registration and delete the dir so the `add` below starts
    // from a clean slate. Both cleanups are best-effort: a `remove` that
    // finds nothing registered is a harmless non-zero exit.
    let _ = shelbi_ssh::run(host, ["git", "-C", &repo, "worktree", "prune"]);

    let has_git = shelbi_ssh::run(host, ["test", "-d", &format!("{wt_str}/.git")])
        .map_err(Error::Io)?
        .status
        .success()
        || shelbi_ssh::run(host, ["test", "-f", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();

    if !has_git {
        let dir_present = shelbi_ssh::run(host, ["test", "-d", &wt_str])
            .map_err(Error::Io)?
            .status
            .success();
        if dir_present {
            let _ = shelbi_ssh::run(
                host,
                ["git", "-C", &repo, "worktree", "remove", "--force", &wt_str],
            );
            let _ = shelbi_ssh::run(host, ["rm", "-rf", &wt_str]);
        }
    }

    let worktree_exists = has_git;

    let branch_exists =
        shelbi_ssh::run(host, ["git", "-C", &repo, "rev-parse", "--verify", branch])
            .map_err(Error::Io)?
            .status
            .success();

    // Every `!branch_exists` path below cuts a fresh branch, so resolve
    // (and fetch) the base up front — and abort the whole sync if the
    // fetch fails, before any worktree/branch state is touched.
    let cut_base = if branch_exists {
        None
    } else {
        Some(resolve_fresh_cut_base(host, &repo, default_branch)?)
    };

    if !worktree_exists {
        // Fresh worktree off the requested branch (or off the default if
        // the branch is also new).
        let mut argv: Vec<String> = vec![
            "git".into(),
            "-C".into(),
            repo.clone(),
            "worktree".into(),
            "add".into(),
        ];
        if let Some(base) = &cut_base {
            // `--no-track`: the base is usually `origin/<default>`, and a
            // task branch must not adopt it as upstream — `git push` /
            // `git status` on the workspace should never point at the
            // default branch.
            argv.push("--no-track".into());
            argv.push("-b".into());
            argv.push(branch.into());
            argv.push(wt_str.clone());
            argv.push(base.clone());
        } else {
            argv.push(wt_str.clone());
            argv.push(branch.into());
        }
        let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: argv.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        return Ok(());
    }

    // Already exists — make sure it's clean and on the right branch. Ignore
    // shelbi's own footprint: `.claude/` deploy files (settings.json,
    // agent-instructions.md, skills/, the review marker) we rewrite on every
    // dispatch, and `.shelbi/` runtime scratch (`.shelbi/messages/` inter-agent
    // mail). Without the carve-out a repo that commits `.claude/` would be
    // permanently "dirty" after the first dispatch, and an idle worktree whose
    // only untracked entry is our own scratch would be wrongly rejected as
    // "uncommitted user work". Mirrors `rebase_workspace_branch_onto_default`.
    let dirty =
        shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain", "-z"])?;
    let user_dirty = user_dirty_porcelain_lines(&dirty);
    if !user_dirty.is_empty() {
        return Err(Error::Other(format!(
            "workspace worktree at {wt_str} has uncommitted changes — \
             commit, stash, or discard before assigning a new task:\n{}",
            user_dirty.join("\n")
        )));
    }

    let current = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    if current.trim() == branch {
        return Ok(());
    }

    // F14: the task's branch may still be checked out in *another*
    // workspace's worktree on this machine (e.g. the task was re-dispatched
    // to a different workspace). Git refuses to check out a branch that's
    // live in another worktree, so `git checkout <branch>` below would die
    // with `fatal: '<branch>' is already checked out`. Detach the branch
    // from any other worktree first. Safe here because we only reach this
    // point when *this* worktree's HEAD is already off `branch`, so the
    // release never touches it.
    if branch_exists {
        release_branch_from_workspace_worktrees(host, project, machine, branch)?;
    }

    // Switch (and create the branch off the freshly-resolved base if it
    // doesn't exist).
    let mut argv: Vec<String> = vec!["git".into(), "-C".into(), wt_str.clone(), "checkout".into()];
    if let Some(base) = &cut_base {
        argv.push("--no-track".into());
        argv.push("-b".into());
        argv.push(branch.into());
        argv.push(base.clone());
    } else {
        argv.push(branch.into());
    }
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Assert the worktree's HEAD is attached to `branch` — the dispatch
/// path's branch-discipline check
/// (bug-worker-commit-landed-on-hub-main-checkout). Runs after
/// [`sync_worktree`], which should already guarantee it, so any failure
/// here means the sync raced another actor or healed into the wrong
/// state; the caller aborts the dispatch before an agent can commit on
/// the wrong ref. A detached HEAD reports as `HEAD` and fails the check
/// too — an agent must never start detached.
pub(crate) fn verify_worktree_on_branch(
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
) -> Result<()> {
    let wt_str = worktree.to_string_lossy().into_owned();
    let head = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    let head = head.trim();
    if head != branch {
        return Err(Error::Other(format!(
            "refusing to dispatch: worktree at {wt_str} has HEAD on `{head}` \
             but the task expects branch `{branch}` — re-run `shelbi task \
             start` after checking the worktree, or fix the checkout manually",
        )));
    }
    Ok(())
}

/// Prepare a workspace's worktree for a **resume** ([`resume_workspace_on_task`])
/// — the preserve-in-flight-work counterpart to [`sync_worktree`].
///
/// The whole point of resume is that the worker's in-flight state survives, so
/// this deliberately does the opposite of `sync_worktree` in two places:
///
/// - It **never bails on a dirty worktree** and **never resets or
///   re-checks-out the branch**. When the worktree already exists (the common
///   case — the tmux session was killed but the worktree is intact), it is left
///   exactly as the worker left it: same branch, same commits, same uncommitted
///   changes. We don't even verify which branch is checked out — forcing it
///   back onto `branch` could stash/lose work, and a resume trusts the tree.
/// - It only touches git when the worktree is **missing entirely** (e.g. a
///   prior teardown removed it, or a dispatch died mid-`worktree add`). Then it
///   recreates the worktree checked out on the task's existing `branch`; if the
///   branch somehow doesn't exist either, it cuts one off a fresh base (same
///   [`resolve_fresh_cut_base`] the dev path uses) so the recreate still
///   succeeds rather than wedging the resume.
///
/// The stale-`.git`-gitlink healing (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F5) is shared with `sync_worktree`: a dir
/// present without a valid `.git` is force-removed and recreated so a later
/// `worktree add` doesn't abort with "already exists".
fn sync_worktree_for_resume(
    host: &Host,
    machine: &Machine,
    worktree: &std::path::Path,
    branch: &str,
    default_branch: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();

    // Prune stale bookkeeping, then heal a present-but-invalid worktree dir
    // (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F5) exactly as `sync_worktree` does. Best-effort.
    let _ = shelbi_ssh::run(host, ["git", "-C", &repo, "worktree", "prune"]);

    let has_git = shelbi_ssh::run(host, ["test", "-d", &format!("{wt_str}/.git")])
        .map_err(Error::Io)?
        .status
        .success()
        || shelbi_ssh::run(host, ["test", "-f", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();

    if has_git {
        // Worktree is intact — preserve it verbatim. This is the whole
        // contract of resume: don't touch the branch, the commits, or the
        // uncommitted changes.
        return Ok(());
    }

    // No valid worktree. If a dir is lingering (created but never got its
    // `.git`), force-remove it so the `worktree add` below starts clean.
    let dir_present = shelbi_ssh::run(host, ["test", "-d", &wt_str])
        .map_err(Error::Io)?
        .status
        .success();
    if dir_present {
        let _ = shelbi_ssh::run(
            host,
            ["git", "-C", &repo, "worktree", "remove", "--force", &wt_str],
        );
        let _ = shelbi_ssh::run(host, ["rm", "-rf", &wt_str]);
    }

    // Recreate the worktree on the task's existing branch. A resume always
    // follows a task that was already in progress, so the branch should exist;
    // fall back to cutting a fresh one off a current base if it somehow
    // doesn't, rather than failing the recreate.
    let branch_exists =
        shelbi_ssh::run(host, ["git", "-C", &repo, "rev-parse", "--verify", branch])
            .map_err(Error::Io)?
            .status
            .success();

    let mut argv: Vec<String> = vec![
        "git".into(),
        "-C".into(),
        repo.clone(),
        "worktree".into(),
        "add".into(),
    ];
    if branch_exists {
        argv.push(wt_str.clone());
        argv.push(branch.into());
    } else {
        let base = resolve_fresh_cut_base(host, &repo, default_branch)?;
        argv.push("--no-track".into());
        argv.push("-b".into());
        argv.push(branch.into());
        argv.push(wt_str.clone());
        argv.push(base);
    }
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Ensure `workspace`'s worktree exists and is checked out on `branch`,
/// creating it (off `default_branch` if the branch is somehow absent) when
/// it's missing — WITHOUT disturbing an existing worktree's in-flight state.
///
/// This is the pane wrapper's safety net against launching the agent in
/// `$HOME` (bug-review-workspace-open-creates-missing-worktree): when a task
/// is assigned to a workspace whose worktree hasn't been created yet — e.g. a
/// review workspace picking up a task that's already in Review — the wrapper
/// calls this before `cd`-ing so the agent starts in the worktree rather than
/// the `cd`-with-no-arg home fallback. It delegates to
/// [`sync_worktree_for_resume`] rather than [`sync_worktree`] so it can't
/// drift from the resume path and, like resume, never resets or bails on an
/// existing worktree — the only case it acts on is a fully-missing one.
pub fn ensure_workspace_worktree(
    project_name: &str,
    machine: &Machine,
    workspace: &WorkspaceSpec,
    branch: &str,
    default_branch: &str,
) -> Result<()> {
    let host = machine.host();
    let worktree = workspace_worktree(machine, workspace);
    // This recovery path sometimes runs without an outer workspace lock. It
    // therefore takes the Git lock directly; callers that do hold a workspace
    // lock already follow the required workspace -> Git ordering.
    let _git_worktree_lock = shelbi_state::lock_git_worktrees(project_name)?;
    sync_worktree_for_resume(&host, machine, &worktree, branch, default_branch)
}

/// If a workspace worktree on this machine is currently on `branch`, switch
/// it to a detached HEAD so the branch ref is free for another worktree to
/// claim. Bails on a dirty workspace worktree (we'd silently lose work).
///
/// [`sync_worktree`] uses this on the dispatch path (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F14): re-dispatching a
/// task whose branch is live in another workspace's worktree would otherwise
/// die on `fatal: '<branch>' is already checked out`. It's safe to call from
/// there because the dispatch only reaches its checkout when the *target*
/// worktree's HEAD is already off `branch`, so this never detaches the
/// worktree it's about to check the branch back out into.
pub(crate) fn release_branch_from_workspace_worktrees(
    host: &Host,
    project: &Project,
    machine: &Machine,
    branch: &str,
) -> Result<()> {
    for workspace in &project.workspaces {
        if workspace.machine != machine.name {
            continue;
        }
        let wt: PathBuf = workspace_worktree(machine, workspace);
        let wt_str = wt.to_string_lossy().into_owned();
        // Skip workspaces without an actual worktree yet.
        let exists = shelbi_ssh::run(host, ["test", "-e", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();
        if !exists {
            continue;
        }
        let head = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
        )?;
        if head.trim() != branch {
            continue;
        }
        let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(Error::Other(format!(
                "workspace `{}`'s worktree is on `{branch}` with uncommitted \
                 changes — commit, stash, or discard first",
                workspace.name
            )));
        }
        // Detach HEAD on the workspace's worktree — frees the branch ref so
        // another worktree can claim it. We avoid switching to a named branch
        // here because the natural choice (`default_branch`) is typically
        // checked out elsewhere, and git refuses to double-claim a branch
        // across worktrees. sync_worktree will re-attach to the right branch
        // the next time the workspace gets a task.
        let out = shelbi_ssh::run(host, ["git", "-C", &wt_str, "checkout", "--detach"])
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("git -C {wt_str} checkout --detach"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }
    Ok(())
}

/// Outcome of [`detach_workspace_worktree`], reported so the caller can emit a
/// single traceable `events.log` line either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachOutcome {
    /// HEAD was pointed at the current commit with the branch ref released.
    /// `from_branch` is the branch the worktree held before (or `None` if it
    /// was already detached — the detach is idempotent).
    Detached { from_branch: Option<String> },
    /// The worktree isn't present on disk (never created, or already torn
    /// down) — there's no branch to free, so this is a benign no-op.
    NoWorktree,
    /// The detach couldn't be performed (probe or `git checkout --detach`
    /// errored). `reason` is a short snippet for the log; the caller keeps the
    /// handoff standing regardless.
    Failed { reason: String },
}

/// Point a workspace worktree's HEAD at its current commit in *detached* state,
/// releasing the branch ref so no worktree holds it. This is the root-cause fix
/// for the post-handoff "branch is already checked out at `<worktree>`" family
/// of failures: git refuses to check out or delete a branch that's live in
/// another worktree, so a finished worker sitting on its task branch blocks the
/// review checkout and the merge / `delete_branch` primitives that come next.
///
/// Detaching *in place* (no target commit) leaves the working tree byte-for-byte
/// unchanged — it only rewrites `HEAD` from `ref: refs/heads/<branch>` to the
/// raw commit sha. The branch ref and every commit on it survive untouched; the
/// branch simply becomes free for another worktree to claim or for a delete to
/// remove. We deliberately do NOT `git checkout <default_branch>` (a named
/// checkout): the default branch is normally live in the hub's own clone, and
/// git won't let two worktrees hold the same branch.
///
/// Idempotent: a worktree whose HEAD is already detached re-detaches cleanly and
/// reports `from_branch: None`. Best-effort by contract — every failure surfaces
/// as [`DetachOutcome::Failed`]/[`DetachOutcome::NoWorktree`] rather than a
/// panic, so a caller on the handoff path can log it and move on without rolling
/// the handoff back.
///
/// A subsequent dispatch onto this workspace re-attaches the worktree to the new
/// task branch via [`sync_worktree`] / [`release_branch_from_workspace_worktrees`],
/// so leaving the idle worktree detached is safe and expected.
pub fn detach_workspace_worktree(host: &Host, worktree: &Path) -> DetachOutcome {
    let wt_str = worktree.to_string_lossy().into_owned();

    // Nothing to free if the worktree was never materialized.
    match shelbi_ssh::run(host, ["test", "-e", &format!("{wt_str}/.git")]) {
        Ok(o) if o.status.success() => {}
        Ok(_) => return DetachOutcome::NoWorktree,
        Err(e) => {
            return DetachOutcome::Failed {
                reason: format!("worktree_probe_failed:{e}"),
            }
        }
    }

    // Record the branch we're releasing, for the event. An already-detached
    // HEAD reports the literal `HEAD`, which we normalize to `None`.
    let from_branch = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|b| !b.is_empty() && b != "HEAD");

    let out = match shelbi_ssh::run(host, ["git", "-C", &wt_str, "checkout", "--detach"]) {
        Ok(o) => o,
        Err(e) => {
            return DetachOutcome::Failed {
                reason: format!("checkout_detach_spawn_failed:{e}"),
            }
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("checkout_detach_failed")
            .to_string();
        return DetachOutcome::Failed { reason };
    }
    DetachOutcome::Detached { from_branch }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec};
    use std::collections::BTreeMap;

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "myapp".into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/myapp".into(),
                    host: None,
                    tags: Vec::new(),
                    forward: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/myapp".into(),
                    host: Some("m2.local".into()),
                    tags: Vec::new(),
                    forward: None,
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "alice".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "bob".into(),
                    machine: "m2".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn local_workspace_lives_in_project_session_window() {
        let p = fixture_project();
        let addr = workspace_tmux_addr(&p, &p.workspaces[0]).unwrap();
        assert_eq!(addr.session, "shelbi-myapp");
        assert_eq!(addr.window, "alice");
    }

    #[test]
    fn remote_workspace_gets_its_own_session() {
        let p = fixture_project();
        let addr = workspace_tmux_addr(&p, &p.workspaces[1]).unwrap();
        assert_eq!(addr.session, "shelbi-w-bob");
        assert_eq!(addr.window, "agent");
    }

    #[test]
    fn worktree_path_under_machine_workdir() {
        let p = fixture_project();
        let wt = workspace_worktree(&p.machines[0], &p.workspaces[0]);
        assert_eq!(wt, PathBuf::from("/tmp/myapp/.shelbi/wt/alice"));
    }

    #[test]
    fn prompt_includes_task_id_branch_and_ready_marker_instruction() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            false,
        );
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("fix-login"));
        assert!(prompt.contains("shelbi/fix-login"));
        // Hands off via the file marker, not the old pane-title / CLI path.
        assert!(prompt.contains(".claude/shelbi-ready"));
        assert!(prompt.contains("printf"));
        assert!(!prompt.contains("shelbi task move"));
        assert!(prompt.contains("\n---\n"));
        // A hook-capable runner (claude) gets no polling instructions.
        assert!(!prompt.contains(".shelbi/messages/"));
        assert!(!prompt.contains("message-ack"));
    }

    #[test]
    fn prompt_falls_back_to_task_id_heading_when_body_empty() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "   ",
            &marker,
            "main",
            "myapp",
            false,
        );
        assert!(prompt.contains("# Task fix-login"));
        assert!(prompt.contains(".claude/shelbi-ready"));
    }

    #[test]
    fn polling_runner_prompt_includes_message_log_instructions() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            true,
        );
        // Still hands off the same way, and still rebases first.
        assert!(prompt.contains(".claude/shelbi-ready"));
        assert!(prompt.contains("git rebase origin/main"));
        // Concrete poll cadence + the per-task log/cursor paths.
        assert!(prompt.contains("After every shell command"));
        assert!(prompt.contains(".shelbi/messages/fix-login.log"));
        assert!(prompt.contains(".shelbi/messages/fix-login.cursor"));
        // Cursor advances past the last-read line (the +1 that avoids a
        // re-delivery off-by-one).
        assert!(prompt.contains("wc -l < .shelbi/messages/fix-login.log"));
        assert!(prompt.contains(") + 1)) > .shelbi/messages/fix-login.cursor"));
        assert!(prompt.contains("tail -n +\"$CURSOR\" .shelbi/messages/fix-login.log"));
        // Ack carries the project + task id and goes to the hub socket.
        assert!(prompt.contains("\"verb\":\"message-ack\""));
        assert!(prompt.contains("\"project\":\"myapp\""));
        assert!(prompt.contains("\"task_id\":\"fix-login\""));
        assert!(prompt.contains("nc -U \"$SHELBI_HUB_SOCK\""));
    }

    #[test]
    fn prompt_instructs_workspace_to_rebase_onto_default_branch_before_marker() {
        // The whole point of this rebase step is to keep the review's base
        // fresh — otherwise tests fail against a stale `default_branch` and
        // the diff_size includes commits that are already on main. Pin the
        // ordering (rebase → marker) and the exact command so a future
        // prompt rewording can't quietly drop the rebase or invert the
        // sequence.
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            false,
        );
        assert!(
            prompt.contains("git fetch origin main && git rebase origin/main"),
            "missing rebase command in prompt: {prompt}"
        );
        let rebase_at = prompt
            .find("git rebase origin/main")
            .expect("rebase command must appear in prompt");
        let marker_at = prompt
            .find("printf '%s\\n'")
            .expect("marker printf must appear in prompt");
        assert!(
            rebase_at < marker_at,
            "rebase must be instructed BEFORE the marker write; prompt: {prompt}"
        );
    }

    #[test]
    fn prompt_uses_projects_default_branch_for_rebase_target() {
        // Not every project's main branch is named `main` — verify the
        // command picks up `default_branch` rather than hard-coding it.
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "trunk",
            "myapp",
            false,
        );
        assert!(
            prompt.contains("git fetch origin trunk && git rebase origin/trunk"),
            "rebase must target the project's default_branch: {prompt}"
        );
        assert!(
            !prompt.contains("origin/main"),
            "stale `main` reference leaked into prompt: {prompt}"
        );
    }

    #[test]
    fn resume_prompt_carries_banner_body_and_handoff() {
        // The resume prompt must (a) announce a resume, (b) keep the task body,
        // and (c) keep the identical rebase + review-marker handoff so the
        // resume path can't drift from the dev-start path on the load-bearing
        // bits.
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_resume_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            false,
            true,
        );
        assert!(
            prompt.contains("Resumed."),
            "missing resume banner: {prompt}"
        );
        assert!(
            prompt.contains("Fix the Safari SSO bug."),
            "body dropped: {prompt}"
        );
        assert!(
            prompt.contains("git fetch origin main && git rebase origin/main"),
            "handoff rebase missing: {prompt}"
        );
        assert!(
            prompt.contains(".claude/shelbi-ready"),
            "marker missing: {prompt}"
        );
        // The banner is a preamble — it precedes the body + handoff.
        let banner_at = prompt.find("Resumed.").unwrap();
        let body_at = prompt.find("Fix the Safari SSO bug.").unwrap();
        assert!(
            banner_at < body_at,
            "banner must precede the body: {prompt}"
        );
    }

    #[test]
    fn resume_prompt_banner_wording_depends_on_conversation_resume() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        // conversation_resumed = true (claude --continue): point at the
        // conversation above.
        let resumed = compose_resume_prompt(
            "t", "shelbi/t", "body", &marker, "main", "myapp", false, true,
        );
        assert!(
            resumed.contains("conversation above"),
            "claude-resume banner should reference the reloaded conversation: {resumed}"
        );
        // conversation_resumed = false (cold relaunch): point at the worktree.
        let cold = compose_resume_prompt(
            "t", "shelbi/t", "body", &marker, "main", "myapp", false, false,
        );
        assert!(
            cold.contains("git log") && cold.contains("git status"),
            "cold-resume banner should tell the worker to inspect its worktree: {cold}"
        );
        assert!(
            !cold.contains("conversation above"),
            "cold resume has no prior conversation to reference: {cold}"
        );
    }

    #[test]
    fn resume_adds_continue_flag_only_for_claude() {
        // `shelbi task resume` builds the launch with resume=true. A claude
        // runner gains `--continue` so it reloads its prior conversation; a
        // non-claude runner has no such flag shelbi drives.
        let p = fixture_project();
        let claude = p.runner("claude").unwrap();
        let launch = workspace_launch_command(claude, &p.workspace_permissions_mode, false, true);
        assert!(
            launch.contains("--continue"),
            "claude resume must add --continue: {launch}"
        );

        let codex = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let launch = workspace_launch_command(&codex, &p.workspace_permissions_mode, false, true);
        assert!(
            !launch.contains("--continue"),
            "non-claude runner must not get --continue: {launch}"
        );
        // Codex is a polling runner: no hook flags are injected. The installed
        // CLI rejects `-c core.hooksPath` under strict validation.
        assert!(
            !launch.contains("core.hooksPath") && !launch.contains("bypass-hook-trust"),
            "codex must not get any hook wiring flags: {launch}"
        );

        // A normal (non-resume) dispatch never adds --continue, even for claude.
        let launch = workspace_launch_command(claude, &p.workspace_permissions_mode, false, false);
        assert!(
            !launch.contains("--continue"),
            "non-resume must not add --continue: {launch}"
        );
    }

    #[test]
    fn codex_launch_uses_initial_prompt_file_without_claude_flags() {
        let codex = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--model".into(), "gpt-5".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let launch = workspace_launch_command_with_startup_prompt(
            &codex,
            "auto",
            true,
            true,
            Some(WORKTREE_STARTUP_PROMPT_REL),
        );
        assert_eq!(
            launch,
            "codex --model gpt-5 \"$(cat .shelbi/startup-prompt.md)\"",
        );
        assert!(
            !launch.contains("--permission-mode"),
            "codex must not receive claude permission flags: {launch}"
        );
        assert!(
            !launch.contains("--continue"),
            "codex must not receive claude resume flags: {launch}"
        );
        assert!(
            !launch.contains("--append-system-prompt"),
            "codex must not receive claude system-prompt flags: {launch}"
        );
    }

    #[test]
    fn claude_cold_launch_uses_initial_prompt_file_and_keeps_system_prompt() {
        let claude = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let launch = workspace_launch_command_with_startup_prompt(
            &claude,
            "auto",
            true,
            false,
            Some(WORKTREE_STARTUP_PROMPT_REL),
        );
        assert_eq!(
            launch,
            "claude --permission-mode auto \
             --append-system-prompt \"$(cat .claude/agent-instructions.md)\" \
             \"$(cat .shelbi/startup-prompt.md)\"",
        );
    }

    #[test]
    fn claude_resume_uses_continue_without_launch_seed_prompt() {
        let claude = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let launch = workspace_launch_command_with_startup_prompt(
            &claude,
            "auto",
            true,
            true,
            Some(WORKTREE_STARTUP_PROMPT_REL),
        );
        assert_eq!(
            launch,
            "claude --permission-mode auto --continue \
             --append-system-prompt \"$(cat .claude/agent-instructions.md)\"",
        );
    }

    #[test]
    fn render_and_deploy_startup_prompt_points_non_claude_at_agent_instructions() {
        let codex = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let rendered = render_startup_prompt("Do the task.\n", true, &codex);
        assert!(
            rendered.starts_with("Read `.claude/agent-instructions.md` first."),
            "non-claude startup prompt must point at deployed agent instructions: {rendered}"
        );
        assert!(rendered.ends_with("Do the task.\n"));

        let tmp = agent_test_tmpdir("startup-prompt");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        deploy_startup_prompt(&Host::Local, &worktree, &rendered).unwrap();
        let dest = worktree.join(WORKTREE_STARTUP_PROMPT_REL);
        assert_eq!(std::fs::read_to_string(dest).unwrap(), rendered);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ready_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_project();
        let marker = workspace_ready_marker(&p.machines[0], &p.workspaces[0]);
        assert_eq!(
            marker,
            PathBuf::from("/tmp/myapp/.shelbi/wt/alice/.claude/shelbi-ready")
        );
    }

    #[test]
    fn read_ready_marker_returns_valid_task_id_and_rejects_garbage() {
        // Local host `cat`s the marker file directly. A well-formed task id
        // round-trips (trimmed); an empty/absent marker is `None`; a body
        // that isn't a valid task id (torn write or hostile content) is an
        // `Err` so the poller leaves the marker in place instead of clearing
        // it on a parse failure.
        let dir = std::env::temp_dir().join(format!(
            "shelbi-review-marker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("shelbi-ready");

        // Absent → None.
        assert!(read_ready_marker(&Host::Local, &marker).unwrap().is_none());

        // Valid id, with the trailing newline the worker writes → trimmed id.
        std::fs::write(&marker, "fix-state-runtime-hardening\n").unwrap();
        assert_eq!(
            read_ready_marker(&Host::Local, &marker).unwrap().as_deref(),
            Some("fix-state-runtime-hardening")
        );

        // Empty (or whitespace-only) → None, not an error.
        std::fs::write(&marker, "\n").unwrap();
        assert!(read_ready_marker(&Host::Local, &marker).unwrap().is_none());

        // A body carrying spaces (a torn write, or an injected value) is not
        // a valid task id → Err, so the caller doesn't clear it.
        std::fs::write(&marker, "fix login now").unwrap();
        assert!(read_ready_marker(&Host::Local, &marker).is_err());

        // A multi-line body (e.g. an OSC-injected second record) also fails
        // validation on the first line's stray content.
        std::fs::write(&marker, "evil id\nsecond line").unwrap();
        assert!(read_ready_marker(&Host::Local, &marker).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Build a project fixture with two `review`-tagged workspaces on `hub`
    /// (plus an untagged slot) so tag-routing paths have something to match.
    fn fixture_with_tagged_workspaces() -> Project {
        let mut p = fixture_project();
        p.workspaces = vec![
            WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            },
            WorkspaceSpec {
                name: "review-1".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: vec!["review".to_string()],
                slot: None,
            },
            WorkspaceSpec {
                name: "review-2".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: vec!["review".to_string()],
                slot: None,
            },
        ];
        p
    }

    #[test]
    fn transition_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_with_tagged_workspaces();
        let r1 = p.workspaces.iter().find(|w| w.name == "review-1").unwrap();
        let marker = workspace_transition_marker(&p.machines[0], r1);
        assert_eq!(
            marker,
            PathBuf::from("/tmp/myapp/.shelbi/wt/review-1/.claude/shelbi-transition")
        );
    }

    #[test]
    fn read_transition_marker_parses_task_and_target_and_rejects_garbage() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-transition-marker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("shelbi-transition");

        // Absent → None.
        assert!(read_transition_marker(&Host::Local, &marker)
            .unwrap()
            .is_none());

        // Two lines (task id + target status), with the trailing newline the
        // worker writes → both trimmed and returned.
        std::fs::write(&marker, "fix-login\nin-progress\n").unwrap();
        assert_eq!(
            read_transition_marker(&Host::Local, &marker).unwrap(),
            Some(TransitionRequest {
                task_id: "fix-login".into(),
                target: "in-progress".into(),
            })
        );

        // A verb target (`reject`) is a valid token here — resolution against
        // the workflow happens poll-side, not in the reader.
        std::fs::write(&marker, "fix-login\nreject\n").unwrap();
        assert_eq!(
            read_transition_marker(&Host::Local, &marker).unwrap(),
            Some(TransitionRequest {
                task_id: "fix-login".into(),
                target: "reject".into(),
            })
        );

        // Blank lines between / around the two records are skipped.
        std::fs::write(&marker, "\nfix-login\n\nin-progress\n\n").unwrap();
        assert_eq!(
            read_transition_marker(&Host::Local, &marker).unwrap(),
            Some(TransitionRequest {
                task_id: "fix-login".into(),
                target: "in-progress".into(),
            })
        );

        // Empty (or whitespace-only) → None, not an error.
        std::fs::write(&marker, "\n").unwrap();
        assert!(read_transition_marker(&Host::Local, &marker)
            .unwrap()
            .is_none());

        // A hostile / torn task id (spaces) fails validation → Err, so the
        // poller leaves the marker in place instead of clearing on a parse fail.
        std::fs::write(&marker, "fix login now\nin-progress\n").unwrap();
        assert!(read_transition_marker(&Host::Local, &marker).is_err());

        // A missing target line → Err (an incomplete request, left in place).
        std::fs::write(&marker, "fix-login\n").unwrap();
        assert!(read_transition_marker(&Host::Local, &marker).is_err());

        // A target with shell-ish / path-ish junk fails the token shape check.
        std::fs::write(&marker, "fix-login\n../etc/passwd\n").unwrap();
        assert!(read_transition_marker(&Host::Local, &marker).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_writes_marker_atomically_via_tmp_and_mv() {
        // The worker must write the marker to a sibling temp file and `mv`
        // it into place — a rename within one directory is atomic, so the
        // poller never `cat`s a half-written body (which would fail
        // `validate_task_id` and stall the handoff).
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix it.",
            &marker,
            "main",
            "myapp",
            false,
        );
        assert!(
            prompt.contains(
                "printf '%s\\n' fix-login > \
                 /work/myapp/.shelbi/wt/alice/.claude/shelbi-ready.tmp && \
                 mv /work/myapp/.shelbi/wt/alice/.claude/shelbi-ready.tmp \
                 /work/myapp/.shelbi/wt/alice/.claude/shelbi-ready"
            ),
            "marker write is not atomic (tmp + mv): {prompt}"
        );
    }

    #[test]
    fn parses_typical_claude_version_output() {
        assert_eq!(
            parse_claude_version("2.1.83 (Claude Code)\n"),
            Some((2, 1, 83))
        );
        assert_eq!(
            parse_claude_version("2.1.153 (Claude Code)"),
            Some((2, 1, 153))
        );
        assert_eq!(parse_claude_version("10.0.0\n"), Some((10, 0, 0)));
    }

    #[test]
    fn rejects_unparseable_version_output() {
        // Empty, garbage, missing patch — never block startup on a parse
        // failure; the caller falls back to a warning + proceed.
        assert_eq!(parse_claude_version(""), None);
        assert_eq!(parse_claude_version("not a version\n"), None);
        assert_eq!(parse_claude_version("2.1\n"), None);
        assert_eq!(parse_claude_version("2.x.83\n"), None);
    }

    #[test]
    fn auto_mode_min_orders_correctly() {
        // Tuple comparison is the whole point of the check — verify it
        // behaves the way the require_… code assumes.
        assert!((2, 1, 83) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 1, 153) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 2, 0) >= CLAUDE_AUTO_MODE_MIN);
        assert!((3, 0, 0) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 1, 82) < CLAUDE_AUTO_MODE_MIN);
        assert!((2, 0, 100) < CLAUDE_AUTO_MODE_MIN);
        assert!((1, 9, 9) < CLAUDE_AUTO_MODE_MIN);
    }

    #[test]
    fn require_auto_mode_no_op_for_non_auto_modes() {
        // Skip the probe entirely if the user picked anything other than
        // `auto` — other modes don't depend on the classifier.
        let runner = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        for mode in ["acceptEdits", "bypassPermissions", "plan", "default"] {
            require_auto_mode_supported(&Host::Local, &runner, mode).unwrap();
        }
    }

    #[test]
    fn spawn_path_injects_permission_mode_for_claude() {
        // Mirror the relevant lines from start_workspace_on_task — the spawn
        // path must compose claude's launch line with --permission-mode so
        // the workspace doesn't depend on settings.json's defaultMode taking
        // effect (which has silently regressed in the past).
        let p = fixture_project(); // workspace_permissions_mode = "auto"
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
    }

    #[test]
    fn spawn_path_passes_through_non_auto_modes() {
        let mut p = fixture_project();
        p.workspace_permissions_mode = "acceptEdits".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode acceptEdits");
    }

    #[test]
    fn spawn_path_omits_flag_for_default_mode() {
        // `default` is claude's own baseline; passing the flag is a no-op
        // that just clutters the command line.
        let mut p = fixture_project();
        p.workspace_permissions_mode = "default".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude");
    }

    #[test]
    fn spawn_path_doesnt_double_flag_when_yaml_already_has_permission_mode() {
        // Repro the real shelbi YAML: `agent_runners.claude.flags:
        // [--permission-mode, auto]` was kept as a pre-bd7a23f quick fix and
        // the spawn-path injection then produced `claude --permission-mode
        // auto --permission-mode auto`. The idempotency check in
        // with_permission_mode must collapse this back to one flag.
        let mut p = fixture_project();
        p.agent_runners.insert(
            "claude".into(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec!["--permission-mode".into(), "auto".into()],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
    }

    #[test]
    fn spawn_path_launches_codex_without_claude_or_hook_flags() {
        // Codex doesn't understand --permission-mode; injecting it would crash
        // the runner on launch. It also gets no hook wiring — the installed CLI
        // rejects `-c core.hooksPath` under strict validation — so the launch
        // is just the runner command plus its own flags.
        let mut p = fixture_project();
        p.agent_runners.insert(
            "codex".into(),
            AgentRunnerSpec {
                command: "codex".into(),
                flags: vec!["--print".into()],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        let runner = p.runner("codex").unwrap().clone();
        let launch = workspace_launch_command(&runner, &p.workspace_permissions_mode, false, false);
        assert_eq!(launch, "codex --print");
        assert!(!launch.contains("--permission-mode"));
        assert!(!launch.contains("core.hooksPath"));
        assert!(!launch.contains("bypass-hook-trust"));
    }

    #[test]
    fn both_host_kinds_construct_the_same_launch_command() {
        // Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F12:
        // the local dispatch path (via the `shelbi open --as-pane`
        // wrapper) and the remote dispatch path (`deploy_and_spawn`'s SSH
        // branch) both build their launch through `workspace_launch_command`.
        // Feed each host's workspace/runner through it and assert the launch
        // string is byte-for-byte identical — one constructor, two hosts, no
        // drift.
        let p = fixture_project(); // permissions_mode = "auto"; both use claude
        let local_ws = &p.workspaces[0]; // alice, Host::Local
        let remote_ws = &p.workspaces[1]; // bob, Host::Ssh
        let local_runner = p.runner(&local_ws.runner).unwrap();
        let remote_runner = p.runner(&remote_ws.runner).unwrap();

        // With an agent deployed: claude gets --permission-mode + the
        // instructions system-prompt. Both hosts produce the same shape.
        let local_launch =
            workspace_launch_command(local_runner, &p.workspace_permissions_mode, true, false);
        let remote_launch =
            workspace_launch_command(remote_runner, &p.workspace_permissions_mode, true, false);
        assert_eq!(local_launch, remote_launch);
        assert_eq!(
            local_launch,
            "claude --permission-mode auto \
             --append-system-prompt \"$(cat .claude/agent-instructions.md)\"",
        );

        // The remote wrapper embeds exactly that shared launch, so the SSH
        // pane execs the same command the local wrapper would.
        let cd_launch = remote_cd_launch(
            &Host::Ssh {
                host: "testhost".into(),
            },
            Path::new("/work/myapp/.shelbi/wt/bob"),
            &remote_launch,
            None,
            &remote_ws.name,
        );
        assert!(
            cd_launch.contains(&remote_launch),
            "remote cd-launch must carry the shared launch verbatim: {cd_launch}"
        );

        // Bare pane (no agent): both hosts still agree — no
        // --append-system-prompt on either.
        let local_bare =
            workspace_launch_command(local_runner, &p.workspace_permissions_mode, false, false);
        let remote_bare =
            workspace_launch_command(remote_runner, &p.workspace_permissions_mode, false, false);
        assert_eq!(local_bare, remote_bare);
        assert_eq!(local_bare, "claude --permission-mode auto");
    }

    #[test]
    fn require_auto_mode_skips_non_claude_runners() {
        // Auto mode is a claude setting; codex / other runners ignore the
        // `defaultMode` key, so probing their `--version` would be both
        // pointless and misleading.
        let runner = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        require_auto_mode_supported(&Host::Local, &runner, "auto").unwrap();
    }

    #[test]
    fn deploy_workspace_settings_writes_local_file_and_creates_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-deploy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let rendered = r#"{"permissions":{"defaultMode":"acceptEdits"}}"#;

        deploy_workspace_settings(&Host::Local, &worktree, rendered).unwrap();

        let settings = worktree.join(".claude/settings.json");
        let actual = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual, rendered);

        // Idempotent: a second call overwrites without error.
        let updated = r#"{"permissions":{"defaultMode":"plan"}}"#;
        deploy_workspace_settings(&Host::Local, &worktree, updated).unwrap();
        let actual2 = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual2, updated);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_runner_hooks_writes_shelbi_owned_neutral_scripts_only() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-runner-hooks-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_runner_hooks(&Host::Local, &worktree).unwrap();

        // The hook scripts are runner-neutral — one copy of each body, no
        // per-runner (`claude.*`/`codex.*`) duplicates.
        let start = worktree.join(".shelbi/hooks/session-start.sh");
        let stop = worktree.join(".shelbi/hooks/stop.sh");
        assert!(start.is_file(), "missing {}", start.display());
        assert!(stop.is_file(), "missing {}", stop.display());
        assert!(
            !worktree.join(".shelbi/hooks/claude.session-start").exists(),
            "per-runner claude.* filenames must no longer be deployed",
        );

        // Codex is a polling runner (no verified hook channel), so no codex.*
        // hook files or the invalid codex.toml are deployed. An earlier version
        // wrote a `.shelbi/hooks/codex.toml` wired via `-c core.hooksPath`, a
        // flag the installed Codex CLI rejects under strict validation.
        assert!(
            !worktree.join(".shelbi/hooks/codex.toml").exists(),
            "codex hooks are not wired; codex.toml must not be deployed",
        );
        assert!(
            !worktree.join(".shelbi/hooks/codex.session-start").exists(),
            "codex hook scripts must not be deployed",
        );

        let start_body = std::fs::read_to_string(&start).unwrap();
        assert!(start_body.contains(".shelbi/messages/$TASK_ID.tail.d"));
        assert!(start_body.contains(".no-task-id.log"));

        let stop_body = std::fs::read_to_string(&stop).unwrap();
        assert!(stop_body.contains("UNREAD=.shelbi/messages/$TASK_ID.unread.log"));
        assert!(stop_body.contains("message-ack"));
        assert!(stop_body.contains("$SHELBI_HUB_SOCK"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&start)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755);
        }

        assert!(
            !worktree.join(".codex").exists(),
            "Shelbi must not create or overwrite user-owned .codex config",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deployed_claude_session_start_hook_starts_tail_and_logs_missing_task_id() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-runner-hooks-exec-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        deploy_runner_hooks(&Host::Local, &worktree).unwrap();

        let script = ".shelbi/hooks/session-start.sh";
        let missing = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env_remove("TASK_ID")
            .current_dir(&worktree)
            .output()
            .expect("run hook without task id");
        assert!(missing.status.success());
        assert!(
            String::from_utf8_lossy(&missing.stderr).contains("TASK_ID unset"),
            "missing-task warning should be visible",
        );
        assert!(worktree.join(".shelbi/messages/.no-task-id.log").is_file());

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("TASK_ID", "feat-x")
            .current_dir(&worktree)
            .output()
            .expect("run hook with task id");
        assert!(out.status.success(), "hook must succeed: {:?}", out.status);

        let pid_path = worktree.join(".shelbi/messages/feat-x.tail.d/pid");
        assert!(pid_path.is_file(), "tail pid file missing");
        let pid = std::fs::read_to_string(&pid_path).unwrap();
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.trim())
            .output();

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn agent_test_tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-agent-deploy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn deploy_agent_instructions_writes_to_claude_dir() {
        // The runner's --append-system-prompt sources from this file; the
        // spawn path's job is to put the agent's instructions.md there
        // verbatim. Pin the destination path so a refactor that moves the
        // deploy footprint can't silently break the runner's loader.
        let tmp = agent_test_tmpdir("instructions");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let body = "# Developer Agent\n\nYou are the developer.\n";

        deploy_agent_instructions(&Host::Local, &worktree, body).unwrap();
        let dest = worktree.join(".claude/agent-instructions.md");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), body);

        // Idempotent — a dispatch on the same workspace overwrites with
        // the new agent's body (e.g. developer → orchestrator).
        let new_body = "# Orchestrator Agent\n";
        deploy_agent_instructions(&Host::Local, &worktree, new_body).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), new_body);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refresh_agent_skills_mirrors_source_and_creates_empty_dir_when_no_src() {
        let tmp = agent_test_tmpdir("skills-empty");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // First case: source skills dir doesn't exist on disk at all.
        // Refresh must still leave `.claude/skills/` in place (empty) so
        // the runner's loader doesn't trip on a missing path.
        let missing_src = tmp.join("agent-with-no-skills/skills");
        refresh_agent_skills(&Host::Local, &worktree, &missing_src).unwrap();
        let dest = worktree.join(".claude/skills");
        assert!(dest.is_dir(), "skills dest must exist even with no source");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);

        // Second case: source has files — they should appear at the dest.
        let src = tmp.join("agent-with-skills/skills");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("greet.md"), "say hi\n").unwrap();
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/inner.md"), "inner skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &src).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("greet.md")).unwrap(),
            "say hi\n",
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("nested/inner.md")).unwrap(),
            "inner skill\n",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refresh_agent_skills_clears_carryover_from_previous_dispatch() {
        // The "different agent on the same workspace" scenario: dispatch
        // A leaves `.claude/skills/a-tool.md`; dispatch B (different
        // agent) must not see A's leftovers. The refresh contract is
        // "destination reflects current source, period."
        let tmp = agent_test_tmpdir("skills-clear");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // Dispatch A.
        let agent_a_skills = tmp.join("agent-a/skills");
        std::fs::create_dir_all(&agent_a_skills).unwrap();
        std::fs::write(agent_a_skills.join("a-tool.md"), "agent A skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_a_skills).unwrap();
        let dest = worktree.join(".claude/skills");
        assert!(dest.join("a-tool.md").exists());

        // Dispatch B with different skills — A's file must be gone.
        let agent_b_skills = tmp.join("agent-b/skills");
        std::fs::create_dir_all(&agent_b_skills).unwrap();
        std::fs::write(agent_b_skills.join("b-tool.md"), "agent B skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_b_skills).unwrap();
        assert!(
            !dest.join("a-tool.md").exists(),
            "agent A's skill must be cleared on dispatch B"
        );
        assert!(dest.join("b-tool.md").exists(), "agent B's skill missing");

        // Dispatch C with no skills at all (or back to A's empty agent)
        // — the dest must be empty afterwards.
        let agent_c_skills = tmp.join("agent-c/skills");
        std::fs::create_dir_all(&agent_c_skills).unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_c_skills).unwrap();
        assert!(dest.is_dir(), "skills dir must persist (just empty)");
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "skills dir must be empty after dispatch C",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Phase 5 acceptance criterion: the line we send into a remote
    /// pane sets `SHELBI_HUB_SOCK` so the agent can write
    /// worker→hub events through the SSH-reverse-forwarded socket.
    /// The default landing path is `/tmp/shelbi-hub-<uid>.sock`.
    #[test]
    fn remote_cd_launch_prefixes_exec_with_hub_socket_env_var() {
        let _g = crate::test_lock::acquire();
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/bob");
        let line = remote_cd_launch(
            &Host::Ssh {
                host: "testhost".into(),
            },
            &wt,
            "claude --permission-mode auto",
            None,
            "bob",
        );
        let expected = shelbi_state::remote_hub_socket_path();
        assert!(
            line.contains(&format!("SHELBI_HUB_SOCK={}", expected.display())),
            "expected default socket path in: {line}"
        );
        // No PORT on the dev path.
        assert!(
            !line.contains("PORT="),
            "dev path must not inject PORT: {line}"
        );
        // Must scope to the exec'd shell, not the surrounding tmux
        // pane — otherwise the `$SHELL -lc` env-strip drops it before
        // claude inherits it.
        let env_at = line.find("SHELBI_HUB_SOCK=").unwrap();
        let exec_at = line.find("exec ").unwrap();
        assert!(
            env_at < exec_at,
            "SHELBI_HUB_SOCK must come BEFORE exec: {line}"
        );
        assert!(
            line.starts_with("cd "),
            "still cd's into the worktree: {line}"
        );
        assert!(line.contains("LANG=C.UTF-8"), "LANG fix preserved: {line}");
    }

    /// `$SHELBI_REMOTE_HUB_SOCK` is the hub-side override knob (per
    /// [`shelbi_state::remote_hub_socket_path`]) — if a user retargets
    /// the reverse forward, the env var injected into the remote pane
    /// must follow.
    #[test]
    fn remote_cd_launch_honors_remote_hub_socket_path_override() {
        let _g = crate::test_lock::acquire();
        std::env::set_var("SHELBI_REMOTE_HUB_SOCK", "/run/user/1000/shelbi.sock");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/bob");
        let line = remote_cd_launch(
            &Host::Ssh {
                host: "testhost".into(),
            },
            &wt,
            "claude",
            None,
            "bob",
        );
        assert!(
            line.contains("SHELBI_HUB_SOCK=/run/user/1000/shelbi.sock"),
            "override not honored: {line}"
        );
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
    }

    /// A TCP-fallback host (Tailscale SSH) gets `SHELBI_HUB_ADDR=tcp:…` and
    /// NO `SHELBI_HUB_SOCK` — there is no usable Unix landing socket there.
    /// A Unix-forward host keeps both (the legacy socket path plus the
    /// scheme-tagged addr) so existing worker snippets are unaffected.
    #[test]
    fn remote_hub_env_prefix_switches_on_persisted_forward_mode() {
        let _g = crate::test_lock::acquire();
        let home =
            std::env::temp_dir().join(format!("shelbi-fwd-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");

        // Default host (no persisted decision) → Unix: both vars present.
        let unix = remote_hub_env_prefix(&Host::Ssh {
            host: "plainbox".into(),
        });
        assert!(
            unix.contains("SHELBI_HUB_ADDR=unix:"),
            "unix host must carry a unix: addr: {unix}"
        );
        assert!(
            unix.contains("SHELBI_HUB_SOCK="),
            "unix host must keep the legacy socket var: {unix}"
        );

        // Persist a TCP decision for a Tailscale-SSH host.
        shelbi_state::save_host_forward(
            "tsbox",
            Some(shelbi_state::HostForward {
                mode: shelbi_core::ForwardMode::Tcp,
                port: Some(47102),
            }),
        )
        .unwrap();
        let tcp = remote_hub_env_prefix(&Host::Ssh {
            host: "tsbox".into(),
        });
        assert!(
            tcp.contains("SHELBI_HUB_ADDR=tcp:127.0.0.1:47102"),
            "tcp host must carry the loopback addr: {tcp}"
        );
        assert!(
            !tcp.contains("SHELBI_HUB_SOCK="),
            "tcp host must NOT set a Unix socket var: {tcp}"
        );

        // Local host has no reverse forward at all → empty prefix.
        assert_eq!(remote_hub_env_prefix(&Host::Local), "");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A dispatch with an explicit `PORT` injects it into the remote pane's
    /// exec env, scoped before `exec` (so the `$SHELL -lc` env-strip doesn't
    /// drop it), alongside the hub socket.
    #[test]
    fn remote_cd_launch_injects_port_when_provided() {
        let _g = crate::test_lock::acquire();
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/slot-1");
        let line = remote_cd_launch(
            &Host::Ssh {
                host: "testhost".into(),
            },
            &wt,
            "claude",
            Some(3010),
            "slot-1",
        );
        assert!(line.contains("PORT=3010"), "expected PORT in: {line}");
        let port_at = line.find("PORT=3010").unwrap();
        let exec_at = line.find("exec ").unwrap();
        assert!(port_at < exec_at, "PORT must come BEFORE exec: {line}");
    }

    #[test]
    fn with_agent_system_prompt_appends_claude_flag_when_agent_set() {
        let runner = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let launch = "claude --permission-mode auto";
        let out = with_agent_system_prompt(launch, Some(WORKTREE_AGENT_INSTRUCTIONS_REL), &runner);
        // The flag reads from the worktree-relative file so it works
        // identically on local and remote hosts (cwd is the worktree).
        assert!(
            out.contains("--append-system-prompt"),
            "missing flag: {out}"
        );
        assert!(
            out.contains("$(cat .claude/agent-instructions.md)"),
            "expected cat substitution in launch line: {out}"
        );
        // The pre-existing flags must survive the append.
        assert!(
            out.starts_with("claude --permission-mode auto"),
            "got: {out}"
        );
    }

    #[test]
    fn with_agent_system_prompt_noop_when_no_agent_or_non_claude() {
        let claude = AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let codex = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
            integration: None,
        };
        let base = "claude --permission-mode auto";

        // No agent → no flag injection (e.g. a test or non-CLI caller
        // that omits the agent context).
        assert_eq!(with_agent_system_prompt(base, None, &claude), base,);

        // Codex doesn't understand the flag; injecting would crash the
        // runner. The agent's instructions.md is still on disk for any
        // future runner-specific loader to pick up.
        assert_eq!(
            with_agent_system_prompt("codex", Some(WORKTREE_AGENT_INSTRUCTIONS_REL), &codex,),
            "codex",
        );
    }

    /// Hub-side fixture: lays out `<SHELBI_HOME>/projects/<p>/agents/
    /// <developer,orchestrator>/{instructions.md,skills/}` so the
    /// deploy_agent_context happy path has something real to read from.
    fn install_default_agents_under_home(home: &Path, project: &str) {
        let dev = home.join(format!("projects/{project}/agents/developer"));
        let orch = home.join(format!("projects/{project}/agents/orchestrator"));
        std::fs::create_dir_all(dev.join("skills")).unwrap();
        std::fs::create_dir_all(orch.join("skills")).unwrap();
        std::fs::write(dev.join("instructions.md"), "# developer\nfix the bug\n").unwrap();
        std::fs::write(orch.join("instructions.md"), "# orchestrator\ncoordinate\n").unwrap();
        // One skill on the developer to prove mounting carries content.
        std::fs::write(dev.join("skills/debug.md"), "# skill: debug\n").unwrap();
    }

    /// Phase 7: when the dispatch resolves to an agent that ships a
    /// per-role `agents/<role>/settings.json`, that file's content is
    /// preferred over the project-wide workspace-settings template.
    /// This is what carries the SessionStart + Stop message-tail hooks
    /// onto the worktree's Claude Code session.
    #[test]
    fn render_workspace_settings_prefers_per_role_when_present() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("render-prefer-role");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "myapp");

        // Per-role settings.json with a unique marker.
        let role_settings = home.join("projects/myapp/agents/developer/settings.json");
        std::fs::write(&role_settings, r#"{"_marker":"from-developer-role"}"#).unwrap();

        let project = fixture_project();
        let rendered =
            render_workspace_settings_preferring_agent(&project, Some("developer")).unwrap();
        assert!(
            rendered.contains("from-developer-role"),
            "per-role settings should win: {rendered}",
        );

        // Without an agent name → project-wide template fallback. The
        // shipped default contains the Phase 7 SessionStart hook string.
        let rendered_fallback = render_workspace_settings_preferring_agent(&project, None).unwrap();
        assert!(
            rendered_fallback.contains("SessionStart"),
            "project-wide default must include SessionStart hook: {rendered_fallback}",
        );
        assert!(
            !rendered_fallback.contains("from-developer-role"),
            "fallback must not see the per-role marker: {rendered_fallback}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `{{workspace_permissions_mode}}` substitution still works for
    /// the per-role file (so a user-authored template with the legacy
    /// placeholder doesn't ship an unsubstituted literal into the
    /// deployed `.claude/settings.json`).
    #[test]
    fn render_workspace_settings_substitutes_placeholder_in_per_role_file() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("render-prefer-role-subst");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "myapp");

        let role_settings = home.join("projects/myapp/agents/developer/settings.json");
        std::fs::write(
            &role_settings,
            r#"{"permissions":{"defaultMode":"{{workspace_permissions_mode}}"}}"#,
        )
        .unwrap();

        let mut project = fixture_project();
        project.workspace_permissions_mode = "acceptEdits".into();
        let rendered =
            render_workspace_settings_preferring_agent(&project, Some("developer")).unwrap();
        assert!(
            rendered.contains(r#""defaultMode":"acceptEdits""#),
            "placeholder must be substituted: {rendered}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_loads_named_agent_into_worktree() {
        // Acceptance criterion (a): the developer agent's instructions.md
        // lands at `.claude/agent-instructions.md` and its skills/ dir
        // mirrors into `.claude/skills/`. Exercises the full hub-side
        // path that the spawn function calls in step 2b.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-developer");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        assert_eq!(
            std::fs::read_to_string(&instructions).unwrap(),
            "# developer\nfix the bug\n",
            "developer's instructions.md must land verbatim in the worktree",
        );
        let skill = worktree.join(".claude/skills/debug.md");
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            "# skill: debug\n",
            "developer's skills/ contents must mirror into .claude/skills/",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Acceptance criterion (a): when `agents/_shared/preamble.md`
    /// exists, the workspace spawn path deploys the agent's
    /// `.claude/agent-instructions.md` with the preamble prepended (and
    /// a blank line separator) — the runner's --append-system-prompt
    /// then loads the composed prompt as one body.
    #[test]
    fn deploy_agent_context_prepends_shared_preamble_when_present() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-preamble");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");
        // Seed the optional shared preamble. The compose pipeline must
        // pick it up without any opt-in flag.
        let preamble_path = shelbi_state::agent_shared_preamble_path("p").unwrap();
        std::fs::create_dir_all(preamble_path.parent().unwrap()).unwrap();
        std::fs::write(&preamble_path, "monorepo overview\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert_eq!(
            body, "monorepo overview\n\n# developer\nfix the bug\n",
            "preamble must lead, blank line separator, then the agent's instructions"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn splice_orchestrator_handoff_wraps_in_system_reminder_block() {
        // The next orchestrator instance reads the handoff as system-
        // reminder context — locked-in shape so a renamer doesn't
        // silently move the marker tags.
        let out = splice_orchestrator_handoff("# orchestrator\nbody\n", "in-flight: nothing\n");
        assert!(out.starts_with("# orchestrator\nbody\n"));
        assert!(out.contains("<system-reminder>\nin-flight: nothing\n</system-reminder>\n"));
    }

    #[test]
    fn splice_orchestrator_handoff_normalises_missing_trailing_newlines() {
        // Either input may lack a trailing newline (the orchestrator
        // wrote without one, or a hand-edited instructions.md was
        // saved without one). The splice has to leave the block tags
        // on their own lines regardless.
        let out = splice_orchestrator_handoff("body", "handoff");
        assert!(out.ends_with("</system-reminder>\n"));
        assert!(out.contains("\n<system-reminder>\nhandoff\n</system-reminder>\n"));
    }

    #[test]
    fn deploy_agent_context_splices_handoff_into_orchestrator_prompt_then_deletes_file() {
        // Acceptance criterion: on orchestrator (re)launch, if
        // `agents/orchestrator/handoff.md` exists, the deploy step
        // splices it in as a <system-reminder> block AND deletes the
        // file so the next launch doesn't double-ingest the same
        // handoff.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-splice");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        // Seed the handoff file the previous instance would have left
        // behind on `shelbi reload` / `shelbi quit`.
        let handoff_path = shelbi_state::orchestrator_handoff_path("p").unwrap();
        std::fs::write(&handoff_path, "watching: task xyz\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            body.contains("# orchestrator\ncoordinate"),
            "orchestrator instructions should still be present: {body}"
        );
        assert!(
            body.contains("<system-reminder>\nwatching: task xyz\n</system-reminder>"),
            "handoff must be spliced as a system-reminder block: {body}"
        );
        assert!(
            !handoff_path.exists(),
            "handoff.md must be deleted after ingestion (one-shot transfer file)"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_skips_handoff_splice_for_non_orchestrator_agents() {
        // A workspace dispatch under the developer agent must NEVER
        // ingest the orchestrator's handoff — that file is private to
        // the dashboard pane, and a leaked splice would dump
        // unrelated state into a worker's prompt.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-dev-skip");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let handoff_path = shelbi_state::orchestrator_handoff_path("p").unwrap();
        std::fs::write(&handoff_path, "private orch state\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            !body.contains("private orch state"),
            "developer dispatch must NOT ingest orchestrator handoff: {body}"
        );
        assert!(
            handoff_path.exists(),
            "developer dispatch must NOT consume the orchestrator's handoff file"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_orchestrator_works_without_handoff_file() {
        // No handoff file is the normal cold-start case (first launch,
        // first reload, etc.). Deploy must not error or splice an
        // empty block.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-none");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            !body.contains("<system-reminder>"),
            "cold start must NOT inject an empty system-reminder block: {body}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_swaps_agents_on_successive_dispatches() {
        // Acceptance criteria (b) + (c) combined: same workspace, two
        // back-to-back dispatches under DIFFERENT agents (the
        // user-owned-status-under-Zen path is one common producer of
        // this). The second dispatch must clear the first's skills and
        // overwrite the instructions.md — the worktree's agent context
        // reflects the *current* agent, not whatever shipped first.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-swap");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // Dispatch 1 — developer.
        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();
        let instructions = worktree.join(".claude/agent-instructions.md");
        let skills_dir = worktree.join(".claude/skills");
        assert!(skills_dir.join("debug.md").exists());
        assert!(std::fs::read_to_string(&instructions)
            .unwrap()
            .contains("developer"));

        // Dispatch 2 — orchestrator. Different agent, no skills of its
        // own. The developer's `debug.md` must NOT survive the swap; the
        // instructions file is overwritten.
        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();
        assert!(
            !skills_dir.join("debug.md").exists(),
            "developer's skill leaked into orchestrator dispatch",
        );
        assert!(
            skills_dir.is_dir(),
            "skills dir must persist after agent swap (just empty)",
        );
        assert!(std::fs::read_to_string(&instructions)
            .unwrap()
            .contains("orchestrator"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod user_shell_tmux_tests {
    //! Real-tmux round-trip for the user-shell slot mark. Skipped silently
    //! when `tmux` isn't on PATH, same doctrine as the other tmux-driven
    //! tests in this workspace.
    use super::*;
    use shelbi_core::Host;

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn kill_session(name: &str) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &format!("={name}")])
            .output();
    }

    /// An unmarked live slot (agent pane / orphaned session) is not a user
    /// shell; a marked one is; a dead slot never is. Exercises the local
    /// window-scoped arm end-to-end so the set-option/show-options wire
    /// shape can't silently drift from what tmux accepts.
    #[test]
    fn user_shell_mark_round_trips_and_dies_with_the_slot() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let session = format!("shelbi-test-usershell-{}", std::process::id());
        kill_session(&session);
        let ok = std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-n",
                "alpha",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to create test session `{session}`");

        let host = Host::Local;
        let addr = TmuxAddr {
            session: session.clone(),
            window: "alpha".into(),
        };

        // Live but unmarked: an agent pane or orphaned session, not a shell.
        assert!(
            !workspace_user_shell_open(&host, &addr).unwrap(),
            "unmarked slot must not read as a user shell"
        );

        mark_user_shell(&host, &addr).expect("set-option should succeed on a live window");
        assert!(
            workspace_user_shell_open(&host, &addr).unwrap(),
            "marked slot must read as a user shell"
        );

        // Slot torn down (user exited the shell): back to plain not-open.
        kill_session(&session);
        assert!(
            !workspace_user_shell_open(&host, &addr).unwrap(),
            "dead slot must not read as a user shell"
        );
    }
}

#[cfg(test)]
mod slot_probe_tests {
    //! Classification tests for the bounded slot probe: the pure reason
    //! helpers, the deadline config clamp, and (when tmux is on PATH) a
    //! real-tmux round-trip of [`probe_workspace_slot`]'s local arm.
    use super::*;
    use shelbi_core::Host;
    use std::time::Duration;

    /// Build a real `Output` with the given exit code — `ExitStatus` has no
    /// public constructor, so we harvest one from a `sh -c "exit N"`.
    fn fake_output(code: i32, stdout: &str, stderr: &str) -> std::process::Output {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .status()
            .expect("sh must run");
        std::process::Output {
            status,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn probe_error_reason_words_the_timeout_for_the_auth_wedge() {
        let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline");
        let reason = probe_error_reason(&timeout, Duration::from_secs(5));
        assert_eq!(
            reason,
            "ssh probe timed out after 5s (interactive auth pending?)"
        );

        // A non-timeout spawn failure keeps its own diagnostic.
        let other = std::io::Error::new(std::io::ErrorKind::NotFound, "no such binary");
        let reason = probe_error_reason(&other, Duration::from_secs(5));
        assert!(reason.contains("no such binary"), "reason: {reason}");
        assert!(!reason.contains("timed out"), "reason: {reason}");
    }

    #[test]
    fn transport_failure_reason_prefers_ssh_stderr_over_exit_status() {
        // ssh's own diagnostic (first non-blank line) is the best reason.
        let out = fake_output(255, "", "\nssh: connect to host devbox port 22: refused\n");
        assert_eq!(
            transport_failure_reason(&out),
            "ssh: connect to host devbox port 22: refused"
        );

        // No stderr at all → fall back to the exit status.
        let out = fake_output(255, "", "");
        assert!(
            transport_failure_reason(&out).contains("255"),
            "reason: {}",
            transport_failure_reason(&out)
        );
    }

    #[test]
    fn user_shell_mark_set_requires_success_and_the_literal_1() {
        assert!(user_shell_mark_set(&fake_output(0, "1\n", "")));
        assert!(!user_shell_mark_set(&fake_output(0, "0\n", "")));
        assert!(!user_shell_mark_set(&fake_output(0, "", "")));
        // Older tmux exits non-zero for an unset user option — plain "not
        // marked", even if something landed on stdout.
        assert!(!user_shell_mark_set(&fake_output(1, "1\n", "")));
    }

    #[test]
    fn probe_deadline_clamps_the_env_override() {
        // NB: env-mutating — this is the only test touching this variable.
        std::env::set_var("SHELBI_PROBE_TIMEOUT_MS", "100");
        assert_eq!(probe_deadline(), Duration::from_millis(500), "clamps low");
        std::env::set_var("SHELBI_PROBE_TIMEOUT_MS", "999999");
        assert_eq!(
            probe_deadline(),
            Duration::from_millis(60_000),
            "clamps high"
        );
        std::env::set_var("SHELBI_PROBE_TIMEOUT_MS", "garbage");
        assert_eq!(
            probe_deadline(),
            Duration::from_millis(DEFAULT_PROBE_TIMEOUT_MS),
            "garbage falls back to the default"
        );
        std::env::remove_var("SHELBI_PROBE_TIMEOUT_MS");
        assert_eq!(
            probe_deadline(),
            Duration::from_millis(DEFAULT_PROBE_TIMEOUT_MS)
        );
    }

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// The local arm answers Dead/Alive with the same semantics as the
    /// unbounded `workspace_slot_alive`, well inside the deadline.
    #[test]
    fn probe_workspace_slot_local_reports_dead_then_alive() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let session = format!("shelbi-test-slotprobe-{}", std::process::id());
        let kill = || {
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &format!("={session}")])
                .output();
        };
        kill();

        let host = Host::Local;
        let addr = TmuxAddr {
            session: session.clone(),
            window: "alpha".into(),
        };
        let deadline = Duration::from_secs(10);

        assert_eq!(
            probe_workspace_slot(&host, &addr, deadline),
            SlotProbe::Dead,
            "no session yet"
        );

        let ok = std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-n",
                "alpha",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to create test session `{session}`");

        assert_eq!(
            probe_workspace_slot(&host, &addr, deadline),
            SlotProbe::Alive { user_shell: false },
            "live unmarked slot"
        );

        kill();
    }
}

#[cfg(test)]
mod rebase_git_tests {
    //! Real-git tests for [`rebase_workspace_branch_onto_default`]. Each test
    //! provisions a tiny on-disk repo with a `main` branch + a feature
    //! branch off it, then exercises one outcome of the rebase function.
    //! Skipped silently if `git` isn't on PATH so the suite still runs on
    //! a git-less sandbox.
    //!
    //! The shape under test is the workspace-side worktree path: in the real
    //! system the worktree shares a `.git` with the project's main clone
    //! (via `git worktree add`), but for the rebase function only the
    //! worktree's own ref/object access matters, so a plain `git init`
    //! repo with a working tree is enough fidelity.
    use super::*;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run_git_in(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(repo);
        for a in args {
            cmd.arg(a);
        }
        // Pin author identity so commit creation works on hosts without
        // a configured user (CI sandboxes, fresh containers). These are
        // process-scoped via env vars, so they don't touch the user's
        // global git config.
        cmd.env("GIT_AUTHOR_NAME", "Shelbi Test");
        cmd.env("GIT_AUTHOR_EMAIL", "test@shelbi.local");
        cmd.env("GIT_COMMITTER_NAME", "Shelbi Test");
        cmd.env("GIT_COMMITTER_EMAIL", "test@shelbi.local");
        cmd.output().expect("git command failed to spawn")
    }

    /// Create a fresh repo with `main` as the default branch and one
    /// committed README so that branching is meaningful. Returns the
    /// repo path.
    fn init_repo(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-rebase-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let init = run_git_in(&dir, &["init", "-q", "-b", "main"]);
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
        let add = run_git_in(&dir, &["add", "README.md"]);
        assert!(add.status.success());
        let commit = run_git_in(&dir, &["commit", "-q", "-m", "initial"]);
        assert!(
            commit.status.success(),
            "initial commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        dir
    }

    fn commit_file(repo: &std::path::Path, name: &str, contents: &str, message: &str) {
        std::fs::write(repo.join(name), contents).unwrap();
        let add = run_git_in(repo, &["add", name]);
        assert!(add.status.success());
        let commit = run_git_in(repo, &["commit", "-q", "-m", message]);
        assert!(
            commit.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&commit.stderr),
        );
    }

    fn head_sha(repo: &std::path::Path) -> String {
        let out = run_git_in(repo, &["rev-parse", "HEAD"]);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn branch_sha(repo: &std::path::Path, branch: &str) -> String {
        let out = run_git_in(repo, &["rev-parse", branch]);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn already_up_to_date_when_branch_contains_default() {
        // Feature branch that's strictly ahead of main → nothing to rebase.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("uptodate");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"])
            .status
            .success()
            .then_some(())
            .expect("branch checkout");
        commit_file(&repo, "feature.txt", "hi\n", "feature work");

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::AlreadyUpToDate { default_sha } => {
                assert_eq!(default_sha, branch_sha(&repo, "main"));
            }
            other => panic!("expected AlreadyUpToDate, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn rebases_cleanly_when_main_advanced_independently() {
        // Branch off main, then advance main with a non-conflicting commit,
        // then run the auto-rebase. The feature branch should be rewritten
        // on top of the new main, with the feature commit preserved.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("clean");

        // Branch off main's current tip.
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "feature\n", "feature work");
        let feature_before = head_sha(&repo);

        // Advance main with a commit on a separate file.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq landed");
        let new_main = head_sha(&repo);

        // Back to the feature branch (this is how the workspace's worktree
        // would be left at the moment the marker fires).
        run_git_in(&repo, &["checkout", "-q", "feature"]);
        assert_eq!(head_sha(&repo), feature_before);

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Rebased {
                before_sha,
                after_sha,
                default_sha,
            } => {
                assert_eq!(before_sha, feature_before);
                assert_eq!(default_sha, new_main);
                assert_ne!(after_sha, feature_before, "HEAD must move");
                // Post-rebase: the feature commit sits on top of new main.
                let parent_of_head =
                    String::from_utf8_lossy(&run_git_in(&repo, &["rev-parse", "HEAD~1"]).stdout)
                        .trim()
                        .to_string();
                assert_eq!(parent_of_head, new_main);
                // Both files survive the rewrite.
                assert!(repo.join("feature.txt").exists());
                assert!(repo.join("prereq.txt").exists());
            }
            other => panic!("expected Rebased, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn conflict_is_aborted_and_branch_head_unchanged() {
        // Both branches touch the same file; the rebase must conflict,
        // we must abort it, and the workspace's branch HEAD must return to
        // its pre-rebase state with a clean worktree. The human reviewer
        // resolves the conflict during the review checkout.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("conflict");

        // Seed a file on main, then branch off.
        commit_file(&repo, "shared.txt", "v0\n", "seed shared");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "shared.txt", "feature change\n", "feature edit");
        let feature_before = head_sha(&repo);

        // Conflicting main commit on the same file.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "shared.txt", "main change\n", "main edit");

        run_git_in(&repo, &["checkout", "-q", "feature"]);
        assert_eq!(head_sha(&repo), feature_before);

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Conflict {
                stderr_excerpt,
                files,
                ..
            } => {
                assert!(
                    !stderr_excerpt.is_empty(),
                    "expected a non-empty conflict excerpt"
                );
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "conflict must name the unmerged file, got {files:?}"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // HEAD must be unchanged after the abort, and the worktree must be
        // clean (no rebase-in-progress state, no merge conflict markers).
        assert_eq!(
            head_sha(&repo),
            feature_before,
            "branch HEAD must roll back after abort"
        );
        let status = run_git_in(&repo, &["status", "--porcelain"]);
        assert!(status.status.success());
        let stdout = String::from_utf8_lossy(&status.stdout);
        assert!(
            stdout.trim().is_empty(),
            "worktree must be clean after abort, got: {stdout}"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn missing_default_branch_is_skipped() {
        // Default-branch name in project YAML doesn't exist in the
        // worktree (renamed branch, typo, fresh shallow clone). Function
        // must skip rather than fail the whole review handoff.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("missing-default");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "hi\n", "feature work");

        let outcome = rebase_workspace_branch_onto_default(
            &Host::Local,
            &repo,
            "ghost-branch-does-not-exist",
        );
        match outcome {
            RebaseOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("ghost-branch-does-not-exist"),
                    "reason should name the missing branch: {reason}"
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_worktree_is_skipped() {
        // Workspace forgot to commit before writing the marker. The function
        // must NOT run the rebase (it would lose the uncommitted work) and
        // must report a skip reason explaining why.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("dirty");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "v0\n", "feature work");

        // Advance main so a rebase would otherwise be needed.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq");
        run_git_in(&repo, &["checkout", "-q", "feature"]);

        // Uncommitted change in the workspace's worktree.
        std::fs::write(repo.join("feature.txt"), "wip change\n").unwrap();

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("dirty"),
                    "reason should mention dirty: {reason}"
                );
            }
            other => panic!("expected Skipped on dirty worktree, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_only_in_dot_claude_is_ignored() {
        // shelbi's own deploy footprint lives under `.claude/` (settings,
        // review marker). It's gitignored in normal use, but if a user
        // hasn't ignored it yet we still don't want it blocking a rebase
        // — same carve-out the review preflight applies.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("dotclaude");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "v0\n", "feature work");

        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq");
        run_git_in(&repo, &["checkout", "-q", "feature"]);

        // Drop a marker-style file under .claude/. It's untracked, but
        // the carve-out skips it from the dirty check.
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(repo.join(".claude/shelbi-ready"), "task-id\n").unwrap();

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        assert!(
            matches!(outcome, RebaseOutcome::Rebased { .. }),
            "expected Rebased despite .claude/ presence, got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_only_in_dot_shelbi_scratch_is_ignored() {
        // shelbi's own runtime scratch (`.shelbi/messages/` inter-agent mail)
        // is written into the worktree root. It is never user work, so a
        // rebase must not skip on it — this is the false positive that logged
        // `dirty_worktree(3_entries)` for `.shelbi/messages/*`.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("dotshelbi");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "v0\n", "feature work");

        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq");
        run_git_in(&repo, &["checkout", "-q", "feature"]);

        // Drop runtime scratch under .shelbi/messages/. Untracked, but ours.
        std::fs::create_dir_all(repo.join(".shelbi/messages")).unwrap();
        std::fs::write(repo.join(".shelbi/messages/a.json"), "{}\n").unwrap();
        std::fs::write(repo.join(".shelbi/messages/b.json"), "{}\n").unwrap();

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        assert!(
            matches!(outcome, RebaseOutcome::Rebased { .. }),
            "expected Rebased despite .shelbi/ scratch, got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn user_dirty_carves_out_shelbi_footprint_but_keeps_real_work() {
        // Records are NUL-delimited (`git status --porcelain -z`).
        // Only shelbi's own paths → clean.
        let scratch = "?? .shelbi/\0?? .shelbi/messages/a.json\0 M .claude/settings.json\0";
        assert!(user_dirty_porcelain_lines(scratch).is_empty());

        // A real edit beside our scratch still counts as dirty.
        let mixed = " M src/lib.rs\0?? .shelbi/messages/a.json\0";
        assert_eq!(user_dirty_porcelain_lines(mixed), vec![" M src/lib.rs"]);

        // A top-level file literally named `.shelbimeta` must NOT be carved
        // out — the prefix guard is `.shelbi/`, not `.shelbi`.
        let lookalike = "?? .shelbimeta\0";
        assert_eq!(
            user_dirty_porcelain_lines(lookalike),
            vec!["?? .shelbimeta"]
        );

        // A clean tree is clean.
        assert!(user_dirty_porcelain_lines("").is_empty());
    }

    #[test]
    fn user_dirty_handles_renames_and_paths_with_spaces() {
        // Under `-z`, a rename is `R  <new>\0<orig>\0`. The origin field must
        // be consumed, not parsed as its own status record, and the carve-out
        // keys off the destination path.
        //
        // - A rename of one shelbi scratch file to another → still carved out.
        let shelbi_rename = "R  .shelbi/b.json\0.shelbi/a.json\0";
        assert!(user_dirty_porcelain_lines(shelbi_rename).is_empty());

        // - A user rename → surfaces the destination record only (the origin
        //   `orig name.rs` is not mis-read as a separate dirty entry).
        let user_rename = "R  new name.rs\0orig name.rs\0";
        assert_eq!(
            user_dirty_porcelain_lines(user_rename),
            vec!["R  new name.rs"]
        );

        // - `-z` never quotes, so a path with a space carves out cleanly
        //   instead of arriving as a quoted string the prefix check misses.
        let spaced_scratch = "?? .claude/a b.txt\0";
        assert!(user_dirty_porcelain_lines(spaced_scratch).is_empty());
    }

    #[test]
    fn detail_format_uses_short_shas() {
        // The detail helper feeds straight into events.log; downstream
        // parsers expect a stable, compact shape — short 7-char SHAs and
        // a recognizable `default=` prefix.
        let outcome = RebaseOutcome::Rebased {
            before_sha: "abcdef0123456789".into(),
            after_sha: "1234567890abcdef".into(),
            default_sha: "fedcba9876543210".into(),
        };
        assert_eq!(outcome.detail(), "abcdef0..1234567_onto_fedcba9");

        let outcome = RebaseOutcome::AlreadyUpToDate {
            default_sha: "abcdef0123456789".into(),
        };
        assert_eq!(outcome.detail(), "default=abcdef0");
    }

    /// The dispatch-path tmux invocation MUST inject `TASK_ID`,
    /// `PROJECT`, and `SHELBI_HUB_SOCK` via tmux `-e` so the pane
    /// wrapper sees them before the caller's state save lands. Without
    /// these `-e` flags the pane wrapper's state lookup races the save
    /// and the Phase 7 message-tail hooks silently no-op — the exact
    /// bug this refactor exists to prevent. See open/pane.rs for the
    /// receiving side.
    #[test]
    fn local_pane_tmux_argv_injects_task_project_and_hub_sock_env_new_session() {
        let argv = local_pane_tmux_argv(LocalPaneTmuxArgs {
            create_new_session: true,
            session: "shelbi-demo",
            window: "alpha",
            task_id: "feat-race",
            project: "demo",
            workspace: "alpha",
            hub_sock: "/tmp/shelbi-hub.sock",
            port: None,
            pane_cmd: "shelbi --project demo open alpha --as-pane",
        });
        assert_eq!(argv[0], "tmux");
        assert_eq!(argv[1], "new-session");
        // Dev path: no PORT injected.
        assert!(
            !argv.iter().any(|s| s.starts_with("PORT=")),
            "dev path must not inject PORT: {argv:?}"
        );
        // `-e KEY=VAL` triplets must appear before the final `sh -c
        // <pane_cmd>` positional; tmux's option parser stops at the
        // first non-flag positional.
        assert!(
            argv.iter().any(|s| s == "TASK_ID=feat-race"),
            "TASK_ID -e missing: {argv:?}"
        );
        assert!(
            argv.iter().any(|s| s == "PROJECT=demo"),
            "PROJECT -e missing: {argv:?}"
        );
        assert!(
            argv.iter()
                .any(|s| s == "SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock"),
            "SHELBI_HUB_SOCK -e missing: {argv:?}"
        );
        // Every `-e` sits directly before its KEY=VAL payload — tmux
        // won't parse `-e` as a flag if the two are split by an
        // unrelated positional.
        for (i, s) in argv.iter().enumerate() {
            if s == "-e" {
                let payload = &argv[i + 1];
                assert!(
                    payload.contains('='),
                    "-e followed by non-KEY=VAL token {payload:?}: {argv:?}"
                );
            }
        }
        // Final positionals are `sh -c <pane_cmd>` — no extra
        // trailing flags that would confuse tmux.
        let last3 = &argv[argv.len() - 3..];
        assert_eq!(last3[0], "sh");
        assert_eq!(last3[1], "-c");
        assert_eq!(last3[2], "shelbi --project demo open alpha --as-pane");
    }

    /// Same env-injection contract for the `new-window` path (project
    /// session already exists — a workspace pane inside an established
    /// dashboard). The KEY=VAL payloads must ride the same `-e` flags.
    #[test]
    fn local_pane_tmux_argv_injects_env_new_window_path() {
        let argv = local_pane_tmux_argv(LocalPaneTmuxArgs {
            create_new_session: false,
            session: "shelbi-demo",
            window: "bravo",
            task_id: "bug-x",
            project: "demo",
            workspace: "bravo",
            hub_sock: "/Users/dev/.shelbi/hub.sock",
            port: None,
            pane_cmd: "shelbi --project demo open bravo --as-pane",
        });
        assert_eq!(argv[0], "tmux");
        assert_eq!(argv[1], "new-window");
        assert!(argv.iter().any(|s| s == "-t"));
        assert!(argv.iter().any(|s| s == "=shelbi-demo:"));
        assert!(argv.iter().any(|s| s == "TASK_ID=bug-x"));
        assert!(argv.iter().any(|s| s == "PROJECT=demo"));
        assert!(argv
            .iter()
            .any(|s| s == "SHELBI_HUB_SOCK=/Users/dev/.shelbi/hub.sock"));
    }

    /// A review workspace pins a deterministic `PORT` via a `-e PORT=<n>`
    /// triplet, riding the same before-the-`sh -c`-positional contract as
    /// the other env vars so tmux parses it as an option, not a command word.
    #[test]
    fn local_pane_tmux_argv_injects_port_for_review_workspace() {
        let argv = local_pane_tmux_argv(LocalPaneTmuxArgs {
            create_new_session: true,
            session: "shelbi-demo",
            window: "review-2",
            task_id: "fix-login",
            project: "demo",
            workspace: "review-2",
            hub_sock: "/tmp/shelbi-hub.sock",
            port: Some(3010),
            pane_cmd: "shelbi --project demo open review-2 --as-pane",
        });
        // `-e PORT=3010` present, and every `-e` still sits directly before a
        // KEY=VAL payload ahead of the final `sh -c` positional.
        let port_at = argv
            .iter()
            .position(|s| s == "PORT=3010")
            .unwrap_or_else(|| panic!("PORT -e missing: {argv:?}"));
        assert_eq!(
            argv[port_at - 1],
            "-e",
            "PORT payload not preceded by -e: {argv:?}"
        );
        let sh_at = argv.iter().position(|s| s == "sh").unwrap();
        assert!(
            port_at < sh_at,
            "PORT must precede the sh -c positional: {argv:?}"
        );
    }

    /// The path half of scp's `host:path` target is re-parsed by the remote
    /// login shell. Replay it through a local `sh -c` (standing in for that
    /// shell) and prove a spaced/metacharacter path lands as one argument
    /// rather than word-splitting.
    #[test]
    fn scp_remote_target_path_survives_remote_shell_as_one_word() {
        let target = scp_remote_target("devbox", "/work/my app/$X/.claude");
        // scp splits on the first `:`; the remote side sees only what follows.
        let (host, path_half) = target.split_once(':').expect("missing host:path split");
        assert_eq!(host, "devbox");
        // `printf '[%s]\n' <path_half>` prints one bracketed line per argument
        // the remote shell tokenized the path into.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '[%s]\\n' {path_half}"))
            .output()
            .expect("sh -c failed");
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[/work/my app/$X/.claude]\n",
            "path word-split or expanded: {path_half}",
        );
    }

    /// Acceptance: `refresh_agent_skills` runs `rm -rf <worktree>/.claude/skills`
    /// over SSH. With a spaced `work_dir` the old (unescaped) wire word-split
    /// into `rm -rf <first> <rest>` and recursively deleted the wrong
    /// directory. Replay the exact wire `shelbi_ssh` now emits through a local
    /// `sh -c` (the stand-in remote shell) against real temp dirs and prove
    /// the sibling that used to be destroyed survives.
    #[test]
    fn refresh_skills_rm_cannot_escape_spaced_worktree() {
        let root = std::env::temp_dir().join(format!("shelbi-rm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Sibling that the buggy word-split (`rm -rf <root>/my`) would nuke.
        let sibling = root.join("my");
        std::fs::create_dir_all(&sibling).unwrap();
        let sentinel = sibling.join("KEEP");
        std::fs::write(&sentinel, b"do not delete").unwrap();
        // The real target: `<root>/my app/.claude/skills`.
        let worktree = root.join("my app");
        let skills = worktree.join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("s.md"), b"x").unwrap();

        // Build the exact argv refresh_agent_skills uses, routed through the
        // SSH transport, then extract the words ssh joins for the remote shell.
        let skills_str = skills.to_string_lossy().into_owned();
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let cmd = shelbi_ssh::build_command(&host, ["rm", "-rf", skills_str.as_str()]);
        let parts: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dd = parts.iter().position(|a| a == "--").expect("no `--`");
        let wire = parts[dd + 1..].join(" ");

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&wire)
            .output()
            .expect("sh -c failed");
        assert!(out.status.success(), "wire: {wire}");

        assert!(
            sentinel.exists(),
            "sibling dir was destroyed by word-split — wire: {wire}",
        );
        assert!(
            !skills.exists(),
            "intended skills dir not removed — wire: {wire}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod sync_worktree_git_tests {
    //! Real-git tests for [`sync_worktree`]'s recovery paths (Shelbi ContextStore
    //! docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F5
    //! partial worktree, F14 branch-checked-out-elsewhere). Each provisions a tiny
    //! on-disk repo whose `work_dir` doubles as the main clone, then drives
    //! `sync_worktree` against `Host::Local`. Skipped when `git` isn't on
    //! PATH so a git-less sandbox still passes.
    use super::*;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec, WorkspaceSpec};
    use std::collections::BTreeMap;

    struct StateHomeGuard {
        previous: Option<std::ffi::OsString>,
        _home: tempfile::TempDir,
    }

    impl StateHomeGuard {
        fn install() -> Self {
            let home = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", home.path());
            Self {
                previous,
                _home: home,
            }
        }
    }

    impl Drop for StateHomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var("SHELBI_HOME", previous),
                None => std::env::remove_var("SHELBI_HOME"),
            }
        }
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run_git_in(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(repo);
        for a in args {
            cmd.arg(a);
        }
        cmd.env("GIT_AUTHOR_NAME", "Shelbi Test");
        cmd.env("GIT_AUTHOR_EMAIL", "test@shelbi.local");
        cmd.env("GIT_COMMITTER_NAME", "Shelbi Test");
        cmd.env("GIT_COMMITTER_EMAIL", "test@shelbi.local");
        cmd.output().expect("git command failed to spawn")
    }

    fn init_repo(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-sync-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(run_git_in(&dir, &["init", "-q", "-b", "main"])
            .status
            .success());
        std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
        assert!(run_git_in(&dir, &["add", "README.md"]).status.success());
        assert!(run_git_in(&dir, &["commit", "-q", "-m", "initial"])
            .status
            .success());
        dir
    }

    /// Project with two hub-local workspaces (`alice`, `bob`) whose worktrees
    /// live under `<repo>/.shelbi/wt/`. `work_dir` is the repo itself.
    fn project_at(repo: &std::path::Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "sync-test".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
                tags: Vec::new(),
                forward: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "alice".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "bob".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn head_of(wt: &std::path::Path) -> String {
        let out = run_git_in(wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn recovers_from_a_partial_worktree_dir() {
        // Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F5:
        // dir exists without a valid `.git` (dispatch killed mid-add).
        // sync_worktree must prune/remove it and add a fresh worktree rather
        // than aborting with "already exists".
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("partial");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);

        // Leave a half-created worktree dir behind (present, no `.git`).
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("stray.txt"), "leftover\n").unwrap();
        assert!(!wt.join(".git").exists());

        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert!(wt.join(".git").exists(), "a valid worktree must now exist");
        assert_eq!(head_of(&wt), "shelbi/x");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn ensure_workspace_worktree_creates_missing_worktree_on_task_branch() {
        let _state_lock = crate::test_lock::acquire();
        let _home = StateHomeGuard::install();
        // bug-review-workspace-open-creates-missing-worktree: a task is
        // assigned to a workspace (e.g. a review workspace on a task already
        // in Review) but the workspace's worktree was never created. The pane
        // wrapper calls `ensure_workspace_worktree` before launching so the
        // agent starts in the worktree rather than $HOME. The task's branch
        // already exists (the task was in progress before Review), so the
        // recovery must check the worktree out onto it.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("ensure-missing");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let workspace = &project.workspaces[0];
        // The task's branch exists on the hub, but no worktree claims it yet.
        assert!(run_git_in(&repo, &["branch", "shelbi/review-me", "main"])
            .status
            .success());
        let wt = workspace_worktree(&machine, workspace);
        assert!(!wt.exists(), "worktree must start missing");

        ensure_workspace_worktree(
            &project.name,
            &machine,
            workspace,
            "shelbi/review-me",
            "main",
        )
        .unwrap();

        assert!(
            wt.join(".git").exists(),
            "a valid worktree must have been created at {}",
            wt.display()
        );
        assert_eq!(
            head_of(&wt),
            "shelbi/review-me",
            "worktree must be checked out on the task branch"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Dispatch branch-discipline check
    /// (bug-worker-commit-landed-on-hub-main-checkout): after sync, the
    /// launch cwd's HEAD must be attached to the task branch. A worktree
    /// on the wrong branch — or detached — fails; the matching branch
    /// passes.
    #[test]
    fn verify_worktree_on_branch_rejects_mismatch_and_detached_head() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("verify-branch");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &wt,
            "shelbi/right",
            "main",
        )
        .unwrap();

        // On the expected branch → passes.
        verify_worktree_on_branch(&Host::Local, &wt, "shelbi/right").unwrap();

        // Expecting a different branch → clear, dispatch-aborting error.
        let err = verify_worktree_on_branch(&Host::Local, &wt, "shelbi/other")
            .expect_err("mismatched branch must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("shelbi/right") && msg.contains("shelbi/other"),
            "error must name both branches: {msg}"
        );

        // Detached HEAD → rejected too; an agent must never start detached.
        assert!(run_git_in(&wt, &["checkout", "--detach"]).status.success());
        verify_worktree_on_branch(&Host::Local, &wt, "shelbi/right")
            .expect_err("detached HEAD must be rejected");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn ensure_workspace_worktree_preserves_existing_worktree() {
        let _state_lock = crate::test_lock::acquire();
        let _home = StateHomeGuard::install();
        // The recovery must never disturb an in-flight worktree: if one
        // already exists (even dirty, even on another branch), `ensure` is a
        // no-op. Otherwise opening a workspace mid-task could reset the branch
        // or lose uncommitted work.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("ensure-preserve");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let workspace = &project.workspaces[0];
        let wt = workspace_worktree(&machine, workspace);
        // Stand up the worktree on one branch, dirty it, then ask ensure for a
        // DIFFERENT branch — it must leave the tree exactly as-is.
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &wt,
            "shelbi/inflight",
            "main",
        )
        .unwrap();
        std::fs::write(wt.join("wip.txt"), "uncommitted\n").unwrap();

        ensure_workspace_worktree(
            &project.name,
            &machine,
            workspace,
            "shelbi/other",
            "main",
        )
        .unwrap();

        assert_eq!(
            head_of(&wt),
            "shelbi/inflight",
            "existing worktree branch must be preserved"
        );
        assert!(
            wt.join("wip.txt").exists(),
            "uncommitted work must be preserved"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn releases_branch_checked_out_in_another_worktree() {
        // Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F14:
        // the requested branch is already checked out in `alice`'s
        // worktree. Dispatching it to `bob` must detach it from `alice`
        // first instead of dying on "already checked out".
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("release");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        assert!(run_git_in(&repo, &["branch", "shelbi/x", "main"])
            .status
            .success());

        // alice takes shelbi/x first.
        let alice_wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &alice_wt,
            "shelbi/x",
            "main",
        )
        .unwrap();
        assert_eq!(head_of(&alice_wt), "shelbi/x");

        // bob is created on its own branch, then re-dispatched onto shelbi/x
        // (which is still live in alice's worktree).
        let bob_wt = workspace_worktree(&machine, &project.workspaces[1]);
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &bob_wt,
            "shelbi/bobinit",
            "main",
        )
        .unwrap();
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &bob_wt,
            "shelbi/x",
            "main",
        )
        .unwrap();

        assert_eq!(head_of(&bob_wt), "shelbi/x", "bob must claim the branch");
        // alice was released (detached) so the branch was free to move.
        assert_ne!(
            head_of(&alice_wt),
            "shelbi/x",
            "alice must have been detached"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn detach_workspace_worktree_frees_branch_preserves_commits_and_allows_reclaim() {
        // The handoff root-cause fix: a finished worker's worktree sitting on
        // its task branch must be detachable so the branch is free for the
        // review checkout / merge / delete. Detaching in place must lose no
        // work (branch ref + commits survive) and leave the branch claimable
        // by another worktree.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("detach-free");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();

        // alice takes shelbi/x and commits real work onto it.
        let alice_wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &alice_wt,
            "shelbi/x",
            "main",
        )
        .unwrap();
        std::fs::write(alice_wt.join("work.txt"), "task output\n").unwrap();
        assert!(run_git_in(&alice_wt, &["add", "work.txt"]).status.success());
        assert!(run_git_in(&alice_wt, &["commit", "-q", "-m", "task work"])
            .status
            .success());
        let tip = String::from_utf8_lossy(&run_git_in(&alice_wt, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();

        let outcome = detach_workspace_worktree(&Host::Local, &alice_wt);
        assert_eq!(
            outcome,
            DetachOutcome::Detached {
                from_branch: Some("shelbi/x".to_string())
            },
            "detach must report the branch it released"
        );

        // HEAD is now detached at the same commit — no branch held.
        assert_eq!(head_of(&alice_wt), "HEAD", "worktree HEAD must be detached");
        assert_eq!(
            String::from_utf8_lossy(&run_git_in(&alice_wt, &["rev-parse", "HEAD"]).stdout).trim(),
            tip,
            "detached HEAD must sit at the former branch tip"
        );

        // The branch ref + its commit survive untouched.
        let branch_sha =
            String::from_utf8_lossy(&run_git_in(&repo, &["rev-parse", "shelbi/x"]).stdout)
                .trim()
                .to_string();
        assert_eq!(branch_sha, tip, "branch ref must still point at the work");
        assert!(
            run_git_in(&repo, &["cat-file", "-e", &format!("{tip}^{{commit}}")])
                .status
                .success(),
            "the branch's commit must still be reachable"
        );

        // The branch is now free: bob can claim it in its own worktree, and it
        // can then be deleted — neither hits `already checked out`.
        let bob_wt = workspace_worktree(&machine, &project.workspaces[1]);
        sync_worktree(
            &project,
            &Host::Local,
            &machine,
            &bob_wt,
            "shelbi/x",
            "main",
        )
        .unwrap();
        assert_eq!(
            head_of(&bob_wt),
            "shelbi/x",
            "bob must claim the freed branch"
        );
        // Free bob too, then the ref deletes cleanly.
        assert_eq!(
            detach_workspace_worktree(&Host::Local, &bob_wt),
            DetachOutcome::Detached {
                from_branch: Some("shelbi/x".to_string())
            }
        );
        assert!(
            run_git_in(&repo, &["branch", "-D", "shelbi/x"])
                .status
                .success(),
            "branch must delete without an `already checked out` error"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn detach_workspace_worktree_is_idempotent_when_already_detached() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("detach-idem");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert!(matches!(
            detach_workspace_worktree(&Host::Local, &wt),
            DetachOutcome::Detached {
                from_branch: Some(_)
            }
        ));
        // Re-detaching an already-detached worktree is a clean no-op with no
        // branch to report.
        assert_eq!(
            detach_workspace_worktree(&Host::Local, &wt),
            DetachOutcome::Detached { from_branch: None }
        );
        assert_eq!(head_of(&wt), "HEAD");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn detach_workspace_worktree_reports_no_worktree_when_missing() {
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("detach-missing");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);
        assert!(!wt.exists(), "worktree must start missing");

        assert_eq!(
            detach_workspace_worktree(&Host::Local, &wt),
            DetachOutcome::NoWorktree,
            "a missing worktree holds no branch — benign no-op"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dispatch_onto_detached_worktree_reattaches_to_new_branch() {
        // After a handoff leaves the worktree detached, the next dispatch onto
        // that idle workspace must still work: sync_worktree re-attaches it to
        // the new task branch from the detached state.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("detach-redispatch");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert!(matches!(
            detach_workspace_worktree(&Host::Local, &wt),
            DetachOutcome::Detached { .. }
        ));
        assert_eq!(head_of(&wt), "HEAD");

        // A fresh dispatch onto a brand-new branch re-attaches cleanly.
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/y", "main").unwrap();
        assert_eq!(
            head_of(&wt),
            "shelbi/y",
            "next dispatch must re-attach the detached worktree to the new branch"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_check_carves_out_shelbi_and_claude_footprint() {
        // F7: shelbi's own `.claude/` (and `.shelbi/`) deploy footprint must
        // not trip the uncommitted-changes gate on the second dispatch.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("carveout");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);

        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        // Simulate shelbi's deploy footprint dirtying the worktree.
        std::fs::create_dir_all(wt.join(".claude/skills")).unwrap();
        std::fs::write(wt.join(".claude/settings.json"), "{}\n").unwrap();
        std::fs::create_dir_all(wt.join(".shelbi")).unwrap();
        std::fs::write(wt.join(".shelbi/note"), "x\n").unwrap();

        // A second dispatch (switching branch) must not bail on the footprint.
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/y", "main").unwrap();
        assert_eq!(head_of(&wt), "shelbi/y");

        // But a genuine user change still blocks.
        std::fs::write(wt.join("user.txt"), "real work\n").unwrap();
        let err = sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main");
        assert!(
            err.is_err(),
            "user-authored change must still block the switch"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resume_preserves_uncommitted_changes_and_branch() {
        // The core resume acceptance: an existing worktree with a committed
        // task branch AND uncommitted changes must be left untouched — no
        // reset, no branch switch, no bail on the dirty tree.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("resume-preserve");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);

        // Stand up the worktree on the task branch and leave in-flight work:
        // one commit plus an uncommitted change.
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();
        std::fs::write(wt.join("committed.txt"), "done\n").unwrap();
        assert!(run_git_in(&wt, &["add", "committed.txt"]).status.success());
        assert!(run_git_in(&wt, &["commit", "-q", "-m", "wip"])
            .status
            .success());
        let committed_sha =
            String::from_utf8_lossy(&run_git_in(&wt, &["rev-parse", "HEAD"]).stdout)
                .trim()
                .to_string();
        // Uncommitted change that a `start`-style clean checkout would reject.
        std::fs::write(wt.join("scratch.txt"), "half-finished\n").unwrap();

        // Resume-sync must be a no-op against the tree.
        sync_worktree_for_resume(&Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert_eq!(head_of(&wt), "shelbi/x", "branch must be preserved");
        assert_eq!(
            String::from_utf8_lossy(&run_git_in(&wt, &["rev-parse", "HEAD"]).stdout).trim(),
            committed_sha,
            "commit must be preserved",
        );
        assert!(
            wt.join("scratch.txt").exists(),
            "uncommitted change must survive a resume",
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("scratch.txt")).unwrap(),
            "half-finished\n",
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resume_recreates_a_missing_worktree_on_the_existing_branch() {
        // Killed/torn-down worktree: the dir is gone but the branch still
        // exists in the repo. Resume-sync must recreate the worktree checked
        // out on that branch (reclaiming the in-flight commits) rather than
        // failing.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("resume-recreate");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        let wt = workspace_worktree(&machine, &project.workspaces[0]);

        // Create the branch with a commit, then remove the worktree entirely.
        sync_worktree(&project, &Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();
        std::fs::write(wt.join("work.txt"), "landed\n").unwrap();
        assert!(run_git_in(&wt, &["add", "work.txt"]).status.success());
        assert!(run_git_in(&wt, &["commit", "-q", "-m", "landed"])
            .status
            .success());
        assert!(run_git_in(
            &repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()]
        )
        .status
        .success());
        assert!(!wt.join(".git").exists(), "worktree should be gone");

        sync_worktree_for_resume(&Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert!(wt.join(".git").exists(), "worktree must be recreated");
        assert_eq!(head_of(&wt), "shelbi/x", "must reclaim the existing branch");
        assert!(
            wt.join("work.txt").exists(),
            "prior commit's file must be present"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }
}

#[cfg(test)]
mod sync_worktree_freshcut_tests {
    //! Real-git tests for [`sync_worktree`]'s fresh-cut base resolution
    //! (the `dispatch-stale-base-branch-cut` fix). Each test provisions a
    //! bare "origin" repo plus a machine clone whose local `main` /
    //! `origin/main` are deliberately N commits behind the bare repo —
    //! exactly the devbox state the bug was observed in — then exercises
    //! one path through `sync_worktree`. Skipped silently if `git` isn't
    //! on PATH so the suite still runs on a git-less sandbox.
    use super::*;
    use shelbi_core::MachineKind;
    use std::path::PathBuf;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run_git_in(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(repo);
        for a in args {
            cmd.arg(a);
        }
        cmd.env("GIT_AUTHOR_NAME", "Shelbi Test");
        cmd.env("GIT_AUTHOR_EMAIL", "test@shelbi.local");
        cmd.env("GIT_COMMITTER_NAME", "Shelbi Test");
        cmd.env("GIT_COMMITTER_EMAIL", "test@shelbi.local");
        cmd.output().expect("git command failed to spawn")
    }

    fn assert_git_ok(out: &std::process::Output, what: &str) {
        assert!(
            out.status.success(),
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    fn fresh_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-synccut-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn commit_file(repo: &std::path::Path, name: &str, contents: &str, message: &str) {
        std::fs::write(repo.join(name), contents).unwrap();
        assert_git_ok(&run_git_in(repo, &["add", name]), "git add");
        assert_git_ok(
            &run_git_in(repo, &["commit", "-q", "-m", message]),
            "git commit",
        );
    }

    fn rev_parse(repo: &std::path::Path, r: &str) -> String {
        let out = run_git_in(repo, &["rev-parse", "--verify", r]);
        assert_git_ok(&out, "git rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn ref_exists(repo: &std::path::Path, r: &str) -> bool {
        run_git_in(repo, &["rev-parse", "--verify", "--quiet", r])
            .status
            .success()
    }

    fn machine_at(repo: &std::path::Path) -> Machine {
        Machine {
            name: "m".into(),
            kind: MachineKind::Local,
            work_dir: repo.to_path_buf(),
            host: None,
            tags: Vec::new(),
            forward: None,
        }
    }

    /// A minimal `Project` wrapping [`machine_at`]. `sync_worktree` only
    /// touches `project` to release the branch from *other* workspace
    /// worktrees (F14); with no workspaces declared that release is a no-op,
    /// so these fresh-cut tests can stay focused on the base-ref resolution.
    fn project_at(repo: &std::path::Path) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            shelbi_core::AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "synccut".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![machine_at(repo)],
            orchestrator: shelbi_core::OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "acceptEdits".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    /// Provision the observed devbox shape: a bare `origin.git`, a machine
    /// clone taken at commit 1, and a writer clone that then pushed commit 2
    /// to the bare repo. The machine clone's local `main` AND `origin/main`
    /// both point at the stale commit — no fetch has run since the clone.
    /// Returns `(root, machine_repo, fresh_sha, stale_sha)`.
    fn stale_clone_fixture(label: &str) -> (PathBuf, PathBuf, String, String) {
        let root = fresh_root(label);
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        assert_git_ok(
            &run_git_in(&seed, &["init", "-q", "-b", "main"]),
            "git init",
        );
        commit_file(&seed, "README.md", "# repo\n", "initial");

        let bare = root.join("origin.git");
        assert_git_ok(
            &run_git_in(
                &root,
                &[
                    "clone",
                    "-q",
                    "--bare",
                    seed.to_str().unwrap(),
                    bare.to_str().unwrap(),
                ],
            ),
            "bare clone",
        );

        let machine = root.join("machine");
        assert_git_ok(
            &run_git_in(
                &root,
                &[
                    "clone",
                    "-q",
                    bare.to_str().unwrap(),
                    machine.to_str().unwrap(),
                ],
            ),
            "machine clone",
        );
        let stale = rev_parse(&machine, "main");

        // Advance origin's main behind the machine clone's back.
        let writer = root.join("writer");
        assert_git_ok(
            &run_git_in(
                &root,
                &[
                    "clone",
                    "-q",
                    bare.to_str().unwrap(),
                    writer.to_str().unwrap(),
                ],
            ),
            "writer clone",
        );
        commit_file(
            &writer,
            "upstream.txt",
            "merged upstream\n",
            "upstream landed",
        );
        assert_git_ok(
            &run_git_in(&writer, &["push", "-q", "origin", "main"]),
            "writer push",
        );
        let fresh = rev_parse(&writer, "main");
        assert_ne!(fresh, stale, "fixture must advance origin past the clone");

        (root, machine, fresh, stale)
    }

    #[test]
    fn fresh_worktree_cut_uses_current_origin_default_not_stale_local() {
        // Acceptance criterion: dispatch to a workspace whose clone is N
        // commits behind cuts the branch from current origin/<default>.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let (root, machine, fresh, stale) = stale_clone_fixture("fresh-wt");
        let wt = root.join("wt-alpha");

        sync_worktree(
            &project_at(&machine),
            &Host::Local,
            &machine_at(&machine),
            &wt,
            "shelbi/task-x",
            "main",
        )
        .expect("sync_worktree should succeed with a reachable origin");

        assert_eq!(
            rev_parse(&machine, "shelbi/task-x"),
            fresh,
            "branch must be cut from freshly-fetched origin/main"
        );
        assert_ne!(rev_parse(&machine, "shelbi/task-x"), stale);
        let out = run_git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_git_ok(&out, "worktree HEAD");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi/task-x");
        // --no-track: a task branch must not adopt origin/main as upstream.
        assert!(
            !run_git_in(
                &wt,
                &["rev-parse", "--abbrev-ref", "shelbi/task-x@{upstream}"]
            )
            .status
            .success(),
            "task branch must not track origin/main"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_worktree_new_branch_cut_uses_current_origin_default() {
        // Same staleness, but through the `checkout -b` path: the
        // worktree already exists from a previous task and just switches.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let (root, machine, fresh, _stale) = stale_clone_fixture("existing-wt");
        let wt = root.join("wt-bravo");
        assert_git_ok(
            &run_git_in(
                &machine,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "shelbi/prev-task",
                    wt.to_str().unwrap(),
                    "main",
                ],
            ),
            "pre-existing worktree",
        );

        sync_worktree(
            &project_at(&machine),
            &Host::Local,
            &machine_at(&machine),
            &wt,
            "shelbi/task-y",
            "main",
        )
        .expect("sync_worktree should succeed with a reachable origin");

        assert_eq!(
            rev_parse(&machine, "shelbi/task-y"),
            fresh,
            "checkout -b path must also cut from freshly-fetched origin/main"
        );
        let out = run_git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_git_ok(&out, "worktree HEAD");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi/task-y");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failing_fetch_aborts_without_cutting() {
        // Acceptance criterion: a failing fetch aborts the dispatch — no
        // branch is cut from the stale ref, no worktree materializes.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let (root, machine, _fresh, _stale) = stale_clone_fixture("fetch-fail");
        // Simulate offline/auth failure: origin exists but is unreachable.
        assert_git_ok(
            &run_git_in(
                &machine,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "/nonexistent/shelbi-gone.git",
                ],
            ),
            "break origin",
        );
        let wt = root.join("wt-charlie");

        let err = sync_worktree(
            &project_at(&machine),
            &Host::Local,
            &machine_at(&machine),
            &wt,
            "shelbi/task-z",
            "main",
        )
        .expect_err("unreachable origin must abort the sync");
        let msg = err.to_string();
        assert!(
            msg.contains("fetch"),
            "error must name the failing fetch: {msg}"
        );
        assert!(
            msg.contains("main"),
            "error must name the base branch: {msg}"
        );
        assert!(
            !ref_exists(&machine, "refs/heads/shelbi/task-z"),
            "no branch may be cut from the stale ref"
        );
        assert!(!wt.exists(), "no worktree may be created on a failed fetch");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_origin_repo_falls_back_to_local_default() {
        // Local-only project (or test fixture) with no `origin` remote:
        // the local default IS the source of truth, so the cut proceeds
        // from it — no fetch, no error.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let root = fresh_root("no-origin");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_git_ok(
            &run_git_in(&repo, &["init", "-q", "-b", "main"]),
            "git init",
        );
        commit_file(&repo, "README.md", "# repo\n", "initial");
        let local_main = rev_parse(&repo, "main");
        let wt = root.join("wt-delta");

        sync_worktree(
            &project_at(&repo),
            &Host::Local,
            &machine_at(&repo),
            &wt,
            "shelbi/task-l",
            "main",
        )
        .expect("no-origin repo must fall back to the local default");
        assert_eq!(rev_parse(&repo, "shelbi/task-l"), local_main);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_branch_needs_no_fetch() {
        // The hub-cut flow: `ensure_branch_for_in_progress` already cut
        // the branch in this repo, so sync just checks it out — even when
        // origin is unreachable, because no fresh cut happens.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let (root, machine, _fresh, stale) = stale_clone_fixture("branch-exists");
        assert_git_ok(
            &run_git_in(&machine, &["branch", "shelbi/pre-cut", "main"]),
            "pre-cut branch",
        );
        assert_git_ok(
            &run_git_in(
                &machine,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "/nonexistent/shelbi-gone.git",
                ],
            ),
            "break origin",
        );
        let wt = root.join("wt-echo");

        sync_worktree(
            &project_at(&machine),
            &Host::Local,
            &machine_at(&machine),
            &wt,
            "shelbi/pre-cut",
            "main",
        )
        .expect("existing branch must check out without fetching");
        assert_eq!(rev_parse(&machine, "shelbi/pre-cut"), stale);
        let out = run_git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_git_ok(&out, "worktree HEAD");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "shelbi/pre-cut"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_failure_emits_dispatch_event_and_aborts_start() {
        // Acceptance criterion: the aborted dispatch is VISIBLE — a
        // `dispatch … status=sync-failed` line lands in events.log, so
        // `shelbi events tail` / the orchestrator see the stall instead of
        // silently getting a stale-based branch.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let _g = crate::test_lock::acquire();
        let (root, machine_repo, _fresh, _stale) = stale_clone_fixture("event");
        assert_git_ok(
            &run_git_in(
                &machine_repo,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "/nonexistent/shelbi-gone.git",
                ],
            ),
            "break origin",
        );
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &home);

        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            shelbi_core::AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        let project = Project {
            name: "synccut".into(),
            repo: machine_repo.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: machine_repo.clone(),
                host: None,
                tags: Vec::new(),
                forward: None,
            }],
            orchestrator: shelbi_core::OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![WorkspaceSpec {
                name: "alice".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            }],
            workspace_poll_interval_secs: 5,
            // NOT "auto": keeps `require_auto_mode_supported` from probing
            // the host's claude binary — this test is about the sync step.
            workspace_permissions_mode: "acceptEdits".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        };

        let result = start_workspace_on_task(StartSpec {
            project: &project,
            workspace: &project.workspaces[0],
            task_id: "task-ev",
            branch: "shelbi/task-ev",
            task_body: "",
            agent: None,
        });

        let events = std::fs::read_to_string(home.join("events.log")).unwrap_or_default();

        match prev_home {
            Some(v) => std::env::set_var("SHELBI_HOME", v),
            None => std::env::remove_var("SHELBI_HOME"),
        }

        assert!(
            result.is_err(),
            "start must abort when the sync fetch fails"
        );
        let line = events
            .lines()
            .find(|l| l.contains("dispatch task=task-ev"))
            .unwrap_or_else(|| panic!("expected a dispatch event line, got: {events}"));
        assert!(line.contains("workspace=alice"), "line: {line}");
        assert!(line.contains("status=sync-failed"), "line: {line}");
        assert!(
            !ref_exists(&machine_repo, "refs/heads/shelbi/task-ev"),
            "no branch may be cut when the dispatch aborts"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
