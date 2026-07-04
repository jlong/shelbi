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
    validate_task_id, Error, Host, Machine, Project, Result, TmuxAddr, WorkspaceSpec,
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
///   `shelbi-review-ready` marker). Normally gitignored, but a repo that
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
    machine.work_dir.join(".shelbi").join("wt").join(&workspace.name)
}

/// The review-ready file marker for a workspace:
/// `<worktree>/.claude/shelbi-review-ready`.
///
/// The workspace writes its current task id here to hand off for review; the
/// hub poller reads it (`stat`/`cat`, local or over SSH), moves the task to
/// the review column, and clears the file. This replaces the old
/// pane-title / `shelbi task move` handoff, both of which raced Claude's own
/// OSC title writes and the Stop hook. A file survives both: nothing the
/// agent's UI does can clobber it.
///
/// It lives under `.claude/` (not the worktree root) on purpose — `.claude/`
/// is where shelbi already deploys `settings.json`, and shelbi relies on it
/// being gitignored so deployed files don't dirty the worktree between
/// tasks. Keeping the marker there means it never shows up in
/// `git status --porcelain` and so never trips [`sync_worktree`]'s
/// clean-worktree check.
pub fn workspace_review_marker(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    workspace_worktree(machine, workspace)
        .join(".claude")
        .join("shelbi-review-ready")
}

/// Read the review-ready marker, returning the task id the workspace wrote into
/// it (trimmed) or `None` if the marker is absent or empty. Works for both
/// local and remote workspaces — `cat` is routed through `shelbi-ssh`, which is
/// a no-op wrapper for [`Host::Local`].
///
/// The marker body is a plain task id, so we validate it against
/// [`validate_task_id`] — the same allowlist task ids pass everywhere else —
/// before returning it. That rejects two failure modes as an `Err` (which the
/// poller logs and, crucially, does *not* clear the marker on, so the signal
/// survives to the next tick): a torn write (a half-flushed id from a
/// non-atomic writer, though workers now write atomically) and a hostile body
/// (e.g. anything but a bare id that a stray program dropped into the file).
/// An invalid body never drives a board move.
pub fn read_review_marker(host: &Host, marker: &Path) -> Result<Option<String>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error for us: the
        // workspace simply hasn't signalled review yet.
        return Ok(None);
    }
    let content = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if content.is_empty() {
        return Ok(None);
    }
    validate_task_id(&content).map_err(|_| {
        Error::Other(format!(
            "review marker at {path} holds an invalid task id ({content:?}); leaving it in place"
        ))
    })?;
    Ok(Some(content))
}

/// Remove the review-ready marker (idempotent — `rm -f` succeeds if absent).
/// Called once the poller has consumed the signal, and again at task start to
/// clear any stale marker before the worktree is reused.
pub fn clear_review_marker(host: &Host, marker: &Path) -> Result<()> {
    let path = marker.to_string_lossy().into_owned();
    shelbi_ssh::run(host, ["rm", "-f", path.as_str()]).map_err(Error::Io)?;
    Ok(())
}

/// The agent-written **transition** marker for a workspace:
/// `<worktree>/.claude/shelbi-transition`.
///
/// Where [`workspace_review_marker`] is the forward-only "I'm done, move me to
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
/// so like the review-ready marker it is a plain file an agent's
/// `instructions.md` can teach it to write. To be torn-write safe, write to a
/// sibling temp path and atomically `mv` it into place, exactly as the
/// review-ready marker prompt does:
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
/// [`read_review_marker`]: routes `cat` through `shelbi-ssh` (a no-op for
/// [`Host::Local`]) and validates the body before returning it.
///
/// Two validations gate the return, and either failing surfaces as an `Err`
/// (which the poller logs and does NOT clear on — the signal survives to the
/// next tick, matching the review marker's torn-write handling) rather than
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
/// `rm -f` is identical to [`clear_review_marker`]'s; kept as a distinct name so
/// call sites read as transition-vs-review explicitly. Called once the poller
/// has consumed the request, and at task start to clear any stale marker before
/// the worktree is reused.
pub fn clear_transition_marker(host: &Host, marker: &Path) -> Result<()> {
    clear_review_marker(host, marker)
}

/// The review-*loaded* file marker for a review workspace:
/// `<worktree>/.claude/shelbi-review-loaded`.
///
/// Distinct from [`workspace_review_marker`] (the dev workspace's
/// review-*ready* handoff). A *review* workspace writes THIS marker once it
/// has loaded the task's branch and booted its dev server — its body carries
/// the task id it loaded plus the URL a human can open (the URL is absent for
/// a diff-only review where nothing runnable was detected). The hub reads it
/// to surface "loaded at <url>" on the board and clears it when the review
/// workspace is reused, exactly as it does the ready marker.
///
/// It lives under `.claude/` for the same reason as the ready marker: that
/// directory is shelbi's gitignored deploy footprint, so the marker never
/// dirties the worktree between tasks and never trips a clean-worktree check.
pub fn workspace_review_loaded_marker(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    workspace_worktree(machine, workspace)
        .join(".claude")
        .join("shelbi-review-loaded")
}

/// Parsed body of a review-loaded marker: the task id the review workspace
/// loaded and the URL it is serving (`None` for a diff-only review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLoaded {
    pub task_id: String,
    pub url: Option<String>,
}

/// Read the review-loaded marker, returning the task id + optional serving
/// URL the review workspace wrote, or `None` if the marker is absent or
/// empty. Mirrors [`read_review_marker`]: routes `cat` through `shelbi-ssh`
/// (a no-op for [`Host::Local`]) and validates the task id against
/// [`validate_task_id`], so a torn or hostile body surfaces as an `Err`
/// (which the poller logs and does NOT clear on — preserving the signal for
/// the next tick) rather than driving a board update off garbage.
///
/// Body contract: a single line, `<task-id>` optionally followed by
/// whitespace and the URL (`fix-login http://host:3000`). A bare `<task-id>`
/// with no URL is a valid diff-only load. Only the first line is read so a
/// multi-line body can't tear across downstream log records.
pub fn read_review_loaded_marker(host: &Host, marker: &Path) -> Result<Option<ReviewLoaded>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error: the review
        // workspace simply hasn't finished loading yet.
        return Ok(None);
    }
    let content = String::from_utf8_lossy(&out.stdout);
    let first = content.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return Ok(None);
    }
    let mut parts = first.splitn(2, char::is_whitespace);
    let task_id = parts.next().unwrap_or("").trim();
    validate_task_id(task_id).map_err(|_| {
        Error::Other(format!(
            "review-loaded marker at {path} holds an invalid task id ({task_id:?}); \
             leaving it in place"
        ))
    })?;
    let url = parts
        .next()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    Ok(Some(ReviewLoaded { task_id: task_id.to_string(), url }))
}

/// Remove the review-loaded marker (idempotent). The `rm -f` is identical to
/// [`clear_review_marker`]'s; kept as a distinct name so call sites read as
/// loaded-vs-ready explicitly.
pub fn clear_review_loaded_marker(host: &Host, marker: &Path) -> Result<()> {
    clear_review_marker(host, marker)
}

/// Deterministic dev-server port for a review workspace:
/// `review.base_port + index * review.port_stride`, where `index` is the
/// workspace's position among its machine's review workspaces (declaration
/// order — see [`Project::review_workspaces`]). With the defaults that's
/// review-1→3000, review-2→3010.
///
/// Returns `None` when `workspace` isn't a review workspace on its machine,
/// so a dev workspace can never be handed a review port by accident.
/// Saturating arithmetic keeps a pathological config from panicking; the
/// per-machine cap ([`shelbi_core::MAX_REVIEW_WORKSPACES_PER_MACHINE`]) means
/// real indices are only 0 or 1.
pub fn review_workspace_port(project: &Project, workspace: &WorkspaceSpec) -> Option<u16> {
    let reviews = project.review_workspaces(&workspace.machine);
    let index = reviews.iter().position(|w| w.name == workspace.name)?;
    let offset = (index as u16).saturating_mul(project.review.port_stride);
    Some(project.review.base_port.saturating_add(offset))
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
    /// Rebase finished cleanly; the workspace's branch is now on top of
    /// `default_sha`. `before_sha`/`after_sha` are HEAD before/after the
    /// rewrite — equal when the rebase ran but produced an empty result
    /// (rare; harmless).
    Rebased {
        before_sha: String,
        after_sha: String,
        default_sha: String,
    },
    /// Rebase ran into conflicts. We aborted it so the worktree returned to
    /// a clean state and the workspace's branch HEAD is unchanged — the human
    /// reviewer will resolve the conflict during the review checkout.
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

/// Rebase the workspace's current branch in `worktree` onto `default_branch`,
/// leaving the worktree on the same branch (now rewritten) on success.
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
/// - On conflict we run `git rebase --abort` and return
///   [`RebaseOutcome::Conflict`] — the workspace's branch HEAD is unchanged.
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
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        Ok(_) | Err(_) => {
            return RebaseOutcome::Skipped {
                reason: format!("default_branch_{default_branch}_not_found"),
            };
        }
    };

    let before_sha = match shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "HEAD"],
    ) {
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

    // 4. Run the rebase. Plain `git rebase` (no autostash — we already
    //    proved the worktree is clean; no `--rebase-merges` — workspaces
    //    produce linear branches).
    let out = match shelbi_ssh::run(host, ["git", "-C", &wt_str, "rebase", default_branch]) {
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
            ["git", "-C", &wt_str, "diff", "--name-only", "--diff-filter=U"],
        )
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

        // Abort so the worktree returns to its pre-rebase state — the
        // workspace's branch HEAD is unchanged and the next `git status` is
        // clean. Abort is best-effort: a hung rebase that won't abort would
        // leave the worktree in an interactive state, but we still want to
        // log the conflict and let the review proceed.
        let _ = shelbi_ssh::run(host, ["git", "-C", &wt_str, "rebase", "--abort"]);
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

/// Does the workspace have a live tmux pane right now?
pub fn workspace_pane_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    // Local: check `session:window` exists. Remote: it's a whole session.
    // `tmux list-windows -t session -F #W | grep -w window` does both.
    let out = shelbi_ssh::run(
        host,
        ["tmux", "list-windows", "-t", &addr.session, "-F", "#W"],
    )
    .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|w| w.trim() == addr.window))
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
    // Tear down any review server pane this workspace owns FIRST. For a
    // local workspace the server pane is a split inside the same window, so
    // the `kill-window` below would take it down anyway — but going through
    // `kill_server_pane` first marks the server's expected-teardown (so the
    // wrapper suppresses its `server_alive=false` event) and clears the
    // record (so the hub stops believing the port is bound). This is what
    // makes re-dispatch onto a review workspace never trip on a still-bound
    // port: every teardown path routes through here. Best-effort — a stuck
    // server teardown must not block the agent-pane kill.
    let _ = crate::server_pane::kill_server_pane(host, workspace_name);

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
            if !workspace_pane_alive(host, addr)? {
                return Ok(());
            }
            // Best-effort — the wrapper's fallback (fire the event with
            // its historical reason) is the pre-fix behavior, so a mark
            // failure just degrades to that.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &addr.target()])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            if !shelbi_tmux::has_session(host, &addr.session)? {
                return Ok(());
            }
            // Remote workspaces don't run the lifecycle wrapper (no
            // shelbi binary on the workspace host), so there's nothing
            // to suppress on that side — but writing the marker is
            // still safe and keeps the API symmetric.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &addr.session])
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
    let marker = workspace_review_marker(&machine, spec.workspace);
    let _ = clear_review_marker(&host, &marker);
    // Same for any stale agent-transition marker — a fresh dispatch must not
    // inherit a bounce request left behind by the previous task on this worktree.
    let _ = clear_transition_marker(&host, &workspace_transition_marker(&machine, spec.workspace));

    // 1. Make sure the worktree exists + is on the right branch, clean.
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
    let marker = workspace_review_marker(&machine, spec.workspace);
    let _ = clear_review_marker(&host, &marker);
    // Same for any stale agent-transition marker — a resumed task is still
    // in-progress, so any transition request present is stale. Best-effort.
    let _ = clear_transition_marker(&host, &workspace_transition_marker(&machine, spec.workspace));

    // Preserve the worktree: don't reset the branch, don't bail on a dirty
    // tree. Only recreate the worktree if it's gone entirely. A failure here
    // aborts before any pane is touched — surface it in events.log so the stall
    // is visible to `shelbi events tail` and the orchestrator, not just the CLI.
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

    // Prefer true conversation-resume for claude; every other runner falls back
    // to plain prompt re-injection (the agent reads its own prior work).
    let resume = shelbi_agent::is_claude_runner(&runner.command);
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

/// Load `spec.branch` onto a **review** workspace and start the `review`
/// agent there to serve it for a human. The review sibling of
/// [`start_workspace_on_task`]:
///
/// - the branch is *moved* onto the review worktree — released from whatever
///   dev worktree produced it, then checked out here. It already exists (a
///   developer cut + committed it), so there's no fresh cut; the machine's
///   top-level clone is never touched (see
///   [`crate::review::move_branch_to_review_worktree`]).
/// - a deterministic `PORT` ([`review_workspace_port`]) is injected into the
///   pane so the review agent's dev server binds a slot that won't collide
///   with a concurrent review workspace on the same machine.
/// - the pane runs the `review` agent (its whole job is loading a branch for
///   a human), unless the caller pins `spec.agent` explicitly (tests).
///
/// The review workspace hands off by writing the *loaded* marker
/// ([`workspace_review_loaded_marker`]) — distinct from the dev review-ready
/// marker; we clear any stale one before reuse, mirroring the dev path.
///
/// Booting the server itself is Phase 4; this gets the review agent running
/// on the review worktree with the right `PORT` and the loaded-marker
/// contract.
pub fn load_review_workspace(spec: StartSpec<'_>) -> Result<TmuxAddr> {
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

    // Serialize against any concurrent start for the same workspace — same
    // rationale as the dev path.
    let _dispatch_lock = shelbi_state::lock_workspace(&spec.project.name, &spec.workspace.name)?;

    require_auto_mode_supported(&host, &runner, &spec.project.workspace_permissions_mode)?;

    // Clear any stale loaded marker from a previous task before the worktree
    // is reused, so the hub can't read an old URL. Best-effort.
    let loaded_marker = workspace_review_loaded_marker(&machine, spec.workspace);
    let _ = clear_review_loaded_marker(&host, &loaded_marker);
    // A review workspace is exactly where a gate agent writes a bounce, so
    // clear any stale transition marker before reusing the worktree too.
    let _ = clear_transition_marker(&host, &workspace_transition_marker(&machine, spec.workspace));

    // 1. Move the branch onto the review worktree (release it from the dev
    //    worktree that produced it, then check it out here). A failure here
    //    aborts before any pane is touched — surface it in events.log so the
    //    stall is visible, not just to the CLI caller.
    if let Err(e) = sync_review_worktree(spec.project, &host, &machine, &worktree, spec.branch) {
        if let Err(log_err) = shelbi_state::append_dispatch_event(
            spec.task_id,
            &spec.workspace.name,
            "sync-failed",
            &e.to_string(),
        ) {
            eprintln!("shelbi: failed to record review-load sync failure in events.log: {log_err}");
        }
        return Err(e);
    }

    // Deterministic dev-server port for this review slot. `None` only if the
    // workspace isn't actually a review workspace on its machine — in which
    // case no PORT is injected (the pane still comes up).
    let port = review_workspace_port(spec.project, spec.workspace);

    // Loading a branch for a human is the review role's whole job, so force
    // the `review` agent unless the caller pinned one explicitly.
    let agent = spec.agent.or(Some(shelbi_state::REVIEW_AGENT));

    let prompt = compose_review_load_prompt(
        spec.task_id,
        spec.branch,
        spec.task_body,
        &loaded_marker,
        port,
    );
    deploy_and_spawn(SpawnArgs {
        project: spec.project,
        workspace: spec.workspace,
        runner: &runner,
        host: &host,
        worktree: &worktree,
        addr: &addr,
        task_id: spec.task_id,
        agent,
        port,
        resume: false,
        prompt: &prompt,
    })?;

    Ok(addr)
}

/// Inputs to [`deploy_and_spawn`] — the shared spawn tail for both the dev
/// ([`start_workspace_on_task`]) and review ([`load_review_workspace`])
/// dispatch paths. The callers differ only in how they prepare the worktree
/// (branch cut vs. branch move), which marker they clear, the `port` they
/// inject, and the `prompt` they send.
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
    /// Injected as `PORT` into the pane env when `Some` (review workspaces).
    port: Option<u16>,
    /// `true` for a `shelbi task resume`: the pane is relaunched WITHOUT
    /// clearing the worktree, and a claude runner reloads its prior
    /// conversation via `--continue`. `false` for a normal (context-clearing)
    /// dispatch.
    resume: bool,
    prompt: &'a str,
}

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
            shelbi_ssh::run_capture(a.host, &argv)?;
        }
        Host::Ssh { .. } => {
            shelbi_tmux::new_session(a.host, &a.addr.session, &a.addr.window, None)?;
            // Remote panes run the agent directly — the lifecycle wrapper isn't
            // deployed on the workspace host — so we build the launch command
            // here and send it into the pane. This goes through the SAME
            // `workspace_launch_command` constructor the local wrapper
            // (`shelbi open --as-pane`) uses, so the two host paths can't drift.
            let launch = workspace_launch_command(
                a.runner,
                &a.project.workspace_permissions_mode,
                a.agent.is_some(),
                a.resume,
            );
            let cd_launch =
                remote_cd_launch(a.worktree, &launch, a.port, &a.workspace.name);
            shelbi_tmux::send_line(a.host, a.addr, &cd_launch)?;
        }
    }

    // 6. Wait until claude has drawn its input box before typing the prompt.
    //    A fixed sleep is fragile: claude's boot time varies with load, and
    //    on a freshly-created worktree it may interpose a "trust this folder"
    //    dialog first (which `wait_for_claude_ready` auto-confirms).
    //
    //    If the probe times out we do NOT fire-and-forget. Typing into a
    //    not-yet-ready UI silently drops the whole prompt: the keystrokes land
    //    nowhere and claude sits at a fresh idle box, yet the old code sent
    //    anyway and the caller then marked the task `in_progress` — stranding a
    //    dead workspace "active" forever with no prompt (observed 2026-07-02 on
    //    alpha). Instead we abort the dispatch cleanly: record an actionable
    //    event and return an error so the caller leaves the task in its
    //    ready-category column for a clean retry.
    if !crate::ready::wait_for_claude_ready(a.host, a.addr, crate::ready::READY_TIMEOUT)? {
        if let Err(e) = shelbi_state::append_dispatch_event(
            a.task_id,
            &a.workspace.name,
            "readiness-timeout",
            "input box not ready",
        ) {
            eprintln!("shelbi: failed to record dispatch readiness-timeout in events.log: {e}");
        }
        return Err(Error::Other(format!(
            "claude readiness probe timed out after {}s on {} — dispatch aborted, \
             prompt NOT sent so the task stays put for retry. Check the workspace \
             pane, then re-run the dispatch.",
            crate::ready::READY_TIMEOUT.as_secs(),
            a.addr.target(),
        )));
    }
    shelbi_tmux::send_line(a.host, a.addr, a.prompt)?;

    // 7. Verify the prompt actually got submitted, not just typed into the
    //    input box. claude's `UserPromptSubmit` hook (see workspace settings
    //    template) writes `\033]2;shelbi:working\007` to the pane title on
    //    every submit — so once any `shelbi:` marker appears in the title,
    //    we know Enter landed. If it doesn't within the window, the most
    //    common cause is that the trailing Enter raced claude's input focus
    //    and was dropped; resend it once and try again.
    //
    //    If it STILL never lands, the prompt is lost — do not mark the task
    //    active. `confirm_prompt_submitted` records a `status=prompt-lost`
    //    dispatch event; we then return an error so the caller leaves the task
    //    in its ready-category column, exactly like a readiness timeout,
    //    instead of moving it to `in_progress` on a workspace that never got
    //    the prompt.
    if !confirm_prompt_submitted(a.host, a.addr, a.task_id, &a.workspace.name, a.prompt) {
        return Err(Error::Other(format!(
            "prompt was not accepted on {} — no submission signal after a retry \
             Enter. Dispatch aborted so the task stays put for retry; check the \
             workspace pane.",
            a.addr.target(),
        )));
    }

    Ok(())
}

/// How long to wait, per attempt, for proof the prompt got submitted (pane
/// title flips to a `shelbi:*` marker OR the pane content shows claude is busy
/// processing). Submit lands almost immediately when the hook fires; the window
/// covers the slow path (busy SSH, sluggish tmux server, a model that takes a
/// few seconds to start streaming). Deliberately longer than the old 5s: a
/// genuine submission whose busy footer was slow to render read as a stall and
/// produced a false `enter-stalled` (observed 2026-07-02 on charlie, whose
/// prompt had submitted fine). With the dispatch now *aborting* on an
/// unconfirmed submit, a false negative is worse than before — so we give a
/// real submission ample room to prove itself.
const PROMPT_SUBMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// How often to re-check the pane while waiting for the submit signal.
const PROMPT_SUBMIT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Scrollback depth captured when checking for the busy signal — enough that a
/// captured pane whose spinner/footer has scrolled a little still shows it.
const PROMPT_SUBMIT_SCROLLBACK: usize = 200;

/// Wait for the prompt-submitted signal; if it doesn't arrive, resend Enter
/// once and wait again. Returns `true` once submission is confirmed. If it is
/// still unconfirmed after the retry, record an actionable `status=prompt-lost`
/// dispatch event (so `shelbi events tail` and the orchestrator see it) and
/// return `false` — the caller aborts the dispatch rather than marking the
/// task active on a workspace sitting on an unsubmitted (or swallowed) prompt.
///
/// Submission is confirmed by any of the signals in `wait_for_prompt_submitted`.
/// The newest — the prompt no longer sitting in claude's input box — is what
/// keeps a genuine submit whose busy footer we never caught (the earliest
/// spinner matches no busy marker) from reading as a lost prompt and aborting
/// a healthy dispatch. The one retry Enter is gated on the prompt *still* being
/// parked in the box: re-Entering an already-cleared box is pointless, and
/// re-Entering a box the user has since started typing into could fire a
/// partial message.
fn confirm_prompt_submitted(
    host: &Host,
    addr: &TmuxAddr,
    task_id: &str,
    workspace: &str,
    prompt: &str,
) -> bool {
    if wait_for_prompt_submitted(host, addr, prompt, PROMPT_SUBMIT_WAIT) {
        return true;
    }
    // No positive signal in the first window. Nudge with one retry Enter only
    // if the prompt is genuinely still parked in the input box — either echoed
    // verbatim or collapsed into a `[Pasted text …]` chip (the auto-restart
    // case, where the first Enter after the paste was dropped). If it's cleared
    // (submitted; busy signal just missed) or we can't see the box, there's
    // nothing a retry would fix.
    if input_holds_unsubmitted_prompt(&shelbi_tmux::capture(host, addr).unwrap_or_default(), prompt)
    {
        // First Enter likely raced claude's focus — resend a bare Enter and
        // give the hook one more window to fire. Avoid spamming Enters: a
        // second one after the prompt is already processed would submit an
        // empty message, which claude ignores, but more than that starts to
        // look like noise.
        if let Err(e) = shelbi_tmux::send_enter(host, addr) {
            eprintln!(
                "shelbi: retry Enter to {} after stalled dispatch failed: {e}",
                addr.target(),
            );
        }
        if wait_for_prompt_submitted(host, addr, prompt, PROMPT_SUBMIT_WAIT) {
            return true;
        }
    }
    eprintln!(
        "shelbi: dispatched prompt to {} but no submission signal appeared \
         after a retry Enter — the prompt was lost; leaving the task unmoved",
        addr.target(),
    );
    if let Err(e) = shelbi_state::append_dispatch_event(
        task_id,
        workspace,
        "prompt-lost",
        "no submit signal after retry",
    ) {
        eprintln!("shelbi: failed to record dispatch prompt-lost in events.log: {e}");
    }
    false
}

/// Poll the workspace's pane until we have proof the prompt got submitted, or
/// `timeout` elapses. Capture failures during the poll are transient (the
/// SSH socket can hiccup); we just ignore them and keep polling.
///
/// Three independent signals — any one is sufficient:
///
/// 1. **Pane title carries a `shelbi:*` marker.** The workspace's
///    `UserPromptSubmit` hook writes `shelbi:working` via OSC, so when the
///    title shows that, Enter definitely landed. The catch is that
///    Claude's own OSC 2 writes (it updates the title with a live
///    activity summary as it works) typically clobber `shelbi:working`
///    within tens of milliseconds — the marker is gone by the time our
///    poll cycle reads it. So we can't rely on this as the only signal.
///
/// 2. **Pane content shows Claude is actively processing.** When the
///    prompt has been submitted and claude is working, the pane renders a
///    spinner line like `⏺ Brewed for 5s · …` or `· Booping… (10s · ↑ 2k
///    tokens)` and an `esc to interrupt` footer — none of which appear in
///    the empty-input / waiting-for-user state. This signal survives
///    Claude's title overwrites because we read the pane *body*, not the
///    title.
///
/// 3. **The input box no longer holds our prompt.** After we type + Enter a
///    prompt, a cleared input box is direct proof it was consumed. This is
///    the signal that closes the false-positive gap: claude's *earliest*
///    spinner (the first second or two, before any tokens stream) matches
///    none of the busy markers in (2), so a prompt that submitted and
///    started working could otherwise slip past both (1) and (2) and get a
///    spurious `enter-stalled`. Reading the box directly doesn't depend on
///    catching the spinner at the right instant. "Cleared" here excludes a
///    collapsed `[Pasted text …]` paste chip ([`input_box_cleared`] /
///    [`input_holds_unsubmitted_prompt`]): a chip is an un-submitted prompt
///    whose body claude never echoes, so counting it as cleared is precisely
///    the auto-restart false positive this guards against.
fn wait_for_prompt_submitted(
    host: &Host,
    addr: &TmuxAddr,
    prompt: &str,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let title = shelbi_tmux::pane_title(host, addr).unwrap_or_default();
        if shelbi_state::parse_pane_title_marker(&title).is_some() {
            return true;
        }
        // Title-marker missed (probably clobbered by claude's own OSC).
        // Fall back to the pane body + a little scrollback — claude's busy
        // spinner / "esc to interrupt" line is a much more durable signal that
        // Enter landed, and the scrollback keeps it visible even if a burst of
        // output has scrolled the footer.
        let screen =
            shelbi_tmux::capture_history(host, addr, PROMPT_SUBMIT_SCROLLBACK).unwrap_or_default();
        if claude_is_processing(&screen) {
            return true;
        }
        // Most direct of all: the prompt is gone from the input box. Guard on
        // the box actually being on screen (`input_box_cleared`) so a capture
        // that missed the box — or one taken before claude echoed the typed
        // prompt — doesn't read as "submitted."
        if input_box_cleared(&screen, prompt) {
            return true;
        }
        std::thread::sleep(PROMPT_SUBMIT_POLL);
    }
    false
}

/// Minimum number of non-whitespace characters a captured input-box line must
/// share with the prompt before we count it as "the prompt is still sitting
/// in the box." Short coincidental overlaps (a lone `git`, a bare `2.`) must
/// not qualify, or claude's dim placeholder — or an unrelated line — could
/// read as an un-submitted prompt.
const PROMPT_ECHO_MIN_MATCH: usize = 24;

/// Extract the lines currently shown inside claude's live input box — the
/// region between the last two horizontal-rule lines at the bottom of the
/// pane — with the leading prompt glyph stripped. Returns `None` when no
/// input box is on screen (a modal dialog, or a capture taken before claude
/// drew its box).
///
/// tmux capture uses `-J`, so tmux's own soft-wraps are already rejoined; the
/// lines we get back are claude's own rendered rows.
fn input_box_lines(screen: &str) -> Option<Vec<String>> {
    // A rule is a pure horizontal border: only box-drawing dashes/corners,
    // and long enough not to be an accidental run. `─── text ───` (claude's
    // titled header rule) contains letters, so it's excluded — we want the
    // plain rules that fence the input box.
    const BORDER: &[char] = &['─', '╭', '╮', '╰', '╯'];
    let is_rule = |l: &&str| {
        let t = l.trim();
        t.chars().count() >= 3 && t.chars().all(|c| BORDER.contains(&c))
    };
    let lines: Vec<&str> = screen.lines().collect();
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| is_rule(l))
        .map(|(i, _)| i)
        .collect();
    if rules.len() < 2 {
        return None;
    }
    let top = rules[rules.len() - 2];
    let bottom = rules[rules.len() - 1];
    Some(
        lines[top + 1..bottom]
            .iter()
            .map(|l| strip_input_glyph(l).trim().to_string())
            .collect(),
    )
}

/// Strip claude's leading input-prompt glyph (`❯` or a plain `>`) plus any
/// following space from a captured input-box line.
fn strip_input_glyph(line: &str) -> &str {
    let t = line.trim_start();
    for g in ['❯', '>'] {
        if let Some(rest) = t.strip_prefix(g) {
            return rest.trim_start();
        }
    }
    t
}

/// Squeeze every whitespace character out of `s`. Comparing prompt text to a
/// captured input box has to survive claude's own soft-wrapping and
/// indentation, which we don't control — dropping all whitespace makes a
/// wrapped row of the prompt a clean substring of the whitespace-free prompt.
fn squeeze_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True when the pane shows claude's input box *still holding the dispatched
/// prompt* — the genuine "un-submitted prompt" state that warrants an
/// `enter-stalled` warning.
///
/// We look only at the live input box, never the scrollback: claude keeps a
/// rendered copy of the prompt as the user message above the box even after a
/// successful submit, so a whole-screen text match would false-positive. We
/// then check whether any single box line reproduces a long-enough slice of
/// the prompt. Matching per-line (rather than the box as a whole) tolerates
/// claude scrolling a tall prompt so only its head or tail is visible, and a
/// truncated middle — any one verbatim prompt line is proof enough.
fn input_holds_prompt(screen: &str, prompt: &str) -> bool {
    let Some(box_lines) = input_box_lines(screen) else {
        return false;
    };
    let prompt_norm = squeeze_ws(prompt);
    box_lines.iter().any(|line| {
        let norm = squeeze_ws(line);
        norm.chars().count() >= PROMPT_ECHO_MIN_MATCH && prompt_norm.contains(&norm)
    })
}

/// True when the input box holds a *collapsed paste chip* — claude renders a
/// large multi-line paste (like the re-injected task prompt: dozens of lines)
/// not by echoing its body but as a single `[Pasted text #1 +45 lines]`
/// placeholder. The pasted prompt is sitting un-submitted in the box, but none
/// of its text is on screen for [`input_holds_prompt`]'s per-line match to
/// catch — so without this the chip reads as a *cleared* box and the dispatch
/// false-confirms a prompt that never went in. This is exactly the auto-restart
/// failure: the pane came up as `❯ [Pasted text #1 +45 lines]`, the Enter that
/// should have submitted it was dropped, and the confirmation could not tell
/// the un-submitted chip apart from a cleared box.
fn input_holds_pasted_chip(screen: &str) -> bool {
    input_box_lines(screen)
        .map(|lines| lines.iter().any(|l| is_pasted_chip(l)))
        .unwrap_or(false)
}

/// True for claude's collapsed-paste placeholder line, e.g.
/// `[Pasted text #1 +45 lines]`. Matched structurally (bracketed, "Pasted
/// text" prefix) rather than by exact wording so a minor label drift across
/// claude versions still registers as "a paste is parked here."
fn is_pasted_chip(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("[Pasted text") && t.ends_with(']')
}

/// True when the input box still holds an un-submitted prompt — either the
/// prompt text is echoed verbatim ([`input_holds_prompt`]) OR claude collapsed
/// a large multi-line paste into a `[Pasted text …]` chip
/// ([`input_holds_pasted_chip`]). Both mean Enter has not landed; the chip case
/// is the one the auto-restart bug hit.
fn input_holds_unsubmitted_prompt(screen: &str, prompt: &str) -> bool {
    input_holds_prompt(screen, prompt) || input_holds_pasted_chip(screen)
}

/// True when claude's input box is on screen but *not* holding the prompt —
/// empty, or showing only its dim placeholder. After we've typed and Enter'd
/// a prompt, a cleared box is direct proof it was consumed (submitted). The
/// `input_box_lines(..).is_some()` guard keeps a capture that missed the box
/// entirely from reading as "cleared." A collapsed paste chip is NOT cleared —
/// it's an un-submitted prompt ([`input_holds_unsubmitted_prompt`]).
fn input_box_cleared(screen: &str, prompt: &str) -> bool {
    input_box_lines(screen).is_some() && !input_holds_unsubmitted_prompt(screen, prompt)
}

/// True when the captured pane shows claude is actively processing a
/// prompt — the prompt-submitted state, as distinct from an empty input
/// box waiting for the user to type something.
///
/// Why these markers are the right ones: each appears ONLY after a
/// prompt has been submitted and claude has started work, and NONE of
/// them appear on the empty-input / ready-for-typing screen. So a match
/// here is sufficient to conclude Enter landed. We avoid keying on the
/// prompt body text (claude's history scrollback contains it in both
/// "submitted" and "still in input" states, depending on how the pane
/// wrapped) and avoid keying on the static input footer (`shift+tab to
/// cycle`, `for shortcuts`) — those persist across both states.
fn claude_is_processing(screen: &str) -> bool {
    // Lowercase compare so "ESC to interrupt" / "esc to interrupt" both
    // match — Claude's footer phrasing has drifted across versions.
    // NB: do NOT add "esc to cancel" here — the trust-this-folder dialog
    // uses that exact string, and we'd otherwise read the dialog as
    // "prompt submitted" before the user has cleared it.
    let lower = screen.to_ascii_lowercase();
    const BUSY_MARKERS: &[&str] = &[
        "esc to interrupt", // claude's "currently working" footer
        "ctrl+c to stop",   // some older versions
        // Claude's spinner line ends with `(<duration> · ↑ <n> tokens)` or
        // `(<duration> · ↓ <n> tokens)` once tokens have streamed. Either
        // direction is proof a prompt got submitted and claude is mid-turn.
        "tokens)",
    ];
    BUSY_MARKERS.iter().any(|m| lower.contains(m))
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
    if !shelbi_agent::is_claude_runner(&runner.command) {
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
        // (F2), route through `run_login_shell_script` so `$SHELL` still
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
    Ok(template.replace("{{workspace_permissions_mode}}", &project.workspace_permissions_mode))
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

/// Relative path (from the worktree root) where the dispatched agent's
/// `skills/` directory is mounted. Claude Code auto-loads any
/// `.claude/skills/` entries on launch.
pub const WORKTREE_AGENT_SKILLS_REL: &str = ".claude/skills";

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
    let mut out =
        String::with_capacity(composed.len() + handoff.len() + 64);
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
pub fn deploy_agent_instructions(
    host: &Host,
    worktree: &Path,
    instructions: &str,
) -> Result<()> {
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
                &Host::Ssh { host: ssh_host.to_string() },
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
    /// `false` → `tmux new-window -d -t <session>: -n <window> …` inside
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
            format!("{}:", a.session),
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
fn remote_cd_launch(worktree: &Path, launch: &str, port: Option<u16>, workspace: &str) -> String {
    let sock = shelbi_state::remote_hub_socket_path();
    // Review workspaces (Some port) also get SHELBI_WORKSPACE so the Review
    // agent's `shelbi workspace serve` resolves its slot.
    let port_env = match port {
        Some(p) => format!("PORT={p} SHELBI_WORKSPACE={ws} ", ws = shelbi_agent::shell_escape(workspace)),
        None => String::new(),
    };
    format!(
        "cd {wd} && LANG=C.UTF-8 {port_env}SHELBI_HUB_SOCK={sock} exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
        wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        sock = shelbi_agent::shell_escape(&sock.to_string_lossy()),
        launch = shelbi_agent::shell_escape(launch),
    )
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
    // `resume` (a `shelbi task resume`) adds `--continue` for a claude runner
    // so the pane reloads its prior conversation instead of starting cold —
    // see [`shelbi_agent::with_continue`]. It's a no-op for a normal dispatch
    // and for non-claude runners.
    let runner_with_mode = shelbi_agent::with_permission_mode(runner, permissions_mode);
    let runner_resolved = shelbi_agent::with_continue(&runner_with_mode, resume);
    with_agent_system_prompt(
        &shelbi_agent::launch_command(&runner_resolved),
        include_agent_instructions.then_some(WORKTREE_AGENT_INSTRUCTIONS_REL),
        runner,
    )
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
    let is_claude = std::path::Path::new(&runner.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude");
    if !is_claude {
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
    scp_text_to_remote(ssh_host, remote_dir, remote_path, rendered, "workspace-settings")
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
        &Host::Ssh { host: ssh_host.to_string() },
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
/// The workspace writes its task id into `<worktree>/.claude/shelbi-review-ready`
/// (see [`workspace_review_marker`]); the hub poller picks it up and moves the
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
    // `validate_task_id` in `read_review_marker` and stall the handoff).
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
         2. Signal that it's ready for review by writing the task id to the \
         review marker file:\n\
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
/// non-hook runners).
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
/// runners without a hook surface (codex, aider, …). Claude Code receives
/// hub messages through its hooks and never sees this section.
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

/// Build the initial prompt for a review-*load* dispatch. The heavy "how to
/// install / serve / auto-detect" guidance lives in the review agent's
/// `instructions.md` (its system prompt) plus the bundled
/// `load-run-detection` skill; this prompt carries only the per-task
/// context: the branch under review, the `PORT` this slot must bind, and the
/// loaded-marker contract the hub watches for.
///
/// Mirrors [`compose_prompt`]'s handoff mechanics — a sibling temp file +
/// atomic `mv` so the hub never `cat`s a half-written marker — but the body
/// carries `<task-id> <url>` (the URL absent for a diff-only review) rather
/// than a bare task id, and there's no rebase step (the branch is loaded
/// as-is for a human to run).
fn compose_review_load_prompt(
    task_id: &str,
    branch: &str,
    body: &str,
    loaded_marker: &Path,
    port: Option<u16>,
) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}\n")
    } else {
        trimmed.to_string()
    };
    let id_esc = shelbi_agent::shell_escape(task_id);
    let marker_esc = shelbi_agent::shell_escape(&loaded_marker.to_string_lossy());
    // Sibling temp file + `mv` so the hub never reads a torn marker — same
    // atomic-rename trick the dev review-ready handoff uses.
    let marker_tmp = {
        let mut s = loaded_marker.as_os_str().to_owned();
        s.push(".tmp");
        PathBuf::from(s)
    };
    let marker_tmp_esc = shelbi_agent::shell_escape(&marker_tmp.to_string_lossy());
    let port_line = match port {
        Some(p) => format!(
            "Your assigned dev-server port is **{p}**, also exported as `PORT` in \
             this pane's environment. Always bind the server to `$PORT` — never a \
             hardcoded port — so concurrent review workspaces don't collide.\n\
             \n"
        ),
        None => String::new(),
    };
    format!(
        "{body_section}\n\n\
         ---\n\
         You are loading task `{task_id}` (branch `{branch}`) onto this review \
         workspace for a human to run. The branch is already checked out in this \
         worktree. Follow your review instructions — install, build, and serve \
         it; do NOT modify code.\n\
         \n\
         {port_line}\
         When the app is serving (or you've determined this is a diff-only \
         review with nothing runnable), write the loaded marker so the hub knows \
         you're ready. Its body is the task id followed by the URL you're serving. \
         Write atomically, filling in the URL you booted:\n\
         \n\
         printf '%s %s\\n' {id_esc} \"$URL\" > {marker_tmp_esc} && mv {marker_tmp_esc} {marker_esc}\n\
         \n\
         For a diff-only review, write just the task id (no URL):\n\
         \n\
         printf '%s\\n' {id_esc} > {marker_tmp_esc} && mv {marker_tmp_esc} {marker_esc}\n\
         \n\
         Also emit your `review_loaded` signal to `$SHELBI_HUB_SOCK` as your \
         instructions describe, then stop and hand off to the human — do not keep \
         editing or start new work."
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
/// finding (orchestrator-zen review F7): the caller must abort the dispatch
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

    // F5: a dispatch killed after the worktree *dir* was created but before
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

    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo, "rev-parse", "--verify", branch],
    )
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
    // from any other worktree first — the same release the review path runs.
    // Safe here because we only reach this point when *this* worktree's HEAD
    // is already off `branch`, so the release never touches it.
    if branch_exists {
        crate::review::release_branch_from_workspace_worktrees(host, project, machine, branch)?;
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
/// The stale-`.git`-gitlink healing (F5) is shared with `sync_worktree`: a dir
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
    // (F5) exactly as `sync_worktree` does. Best-effort.
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
    let branch_exists = shelbi_ssh::run(host, ["git", "-C", &repo, "rev-parse", "--verify", branch])
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

/// Ensure a *review* workspace's worktree exists and has `branch` checked
/// out, moving the branch off whatever dev worktree produced it. The review
/// analogue of [`sync_worktree`], with one load-bearing difference: a review
/// load never cuts a fresh branch. It always follows a dev task that already
/// created + committed `branch`, so an absent branch is an error, not a cue
/// to branch off the default.
///
/// The branch is *moved*, not copied: [`crate::review::move_branch_to_review_worktree`]
/// releases (detaches) whichever dev worktree currently holds it before
/// checking it out here, so git will let the review worktree claim the ref.
/// The machine's top-level clone is never touched — only workspace worktrees
/// swap the branch.
fn sync_review_worktree(
    project: &Project,
    host: &Host,
    machine: &Machine,
    worktree: &Path,
    branch: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();

    // Prune stale bookkeeping and heal a present-but-invalid worktree dir,
    // exactly as `sync_worktree` does on the dev path (F5): a dispatch killed
    // after the dir was created but before its `.git` gitlink landed leaves
    // `<wt>` present-but-invalid, and every later `worktree add` aborts with
    // "already exists". Both cleanups are best-effort.
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

    // A review load loads a branch a developer already produced — it must
    // exist. If it doesn't, fail loudly rather than silently cutting one off
    // the default (which would serve empty/wrong changes to the reviewer).
    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo, "rev-parse", "--verify", branch],
    )
    .map_err(Error::Io)?
    .status
    .success();
    if !branch_exists {
        return Err(Error::Other(format!(
            "review load for branch `{branch}` found no such branch in {repo} — \
             a review workspace loads a branch a developer already produced, so \
             there is nothing to check out"
        )));
    }

    if !worktree_exists {
        // Fresh review worktree. Create it *detached* at the branch's commit
        // so the `worktree add` never collides with the branch that may still
        // be checked out in the dev worktree; the move below then attaches
        // this worktree to the branch (after releasing the dev worktree).
        let argv: Vec<String> = vec![
            "git".into(),
            "-C".into(),
            repo.clone(),
            "worktree".into(),
            "add".into(),
            "--detach".into(),
            wt_str.clone(),
            branch.into(),
        ];
        let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: argv.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    } else {
        // Existing worktree — make sure it's clean before we switch branches.
        // Carve out shelbi's own footprint (`.shelbi/` metadata + the
        // `.claude/` deploy files we rewrite every dispatch), same as
        // `sync_worktree` and `preflight_workdir`.
        let dirty = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "status", "--porcelain", "-z"],
        )?;
        let user_dirty = user_dirty_porcelain_lines(&dirty);
        if !user_dirty.is_empty() {
            return Err(Error::Other(format!(
                "review workspace worktree at {wt_str} has uncommitted changes — \
                 commit, stash, or discard before loading a new task:\n{}",
                user_dirty.join("\n")
            )));
        }

        // Already on the requested branch (a follow-up load of the same
        // task)? Nothing to move — the fresh pane below re-serves it.
        let current = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
        )?;
        if current.trim() == branch {
            return Ok(());
        }
    }

    // Release the branch from any dev worktree holding it, then check it out
    // here. For the fresh-worktree case this attaches the detached HEAD onto
    // the branch; for the existing case it's the actual switch. The review
    // worktree is not on `branch` at this point (guarded above), so the
    // release never detaches the worktree we're about to check it out into.
    crate::review::move_branch_to_review_worktree(host, project, machine, worktree, branch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::model::WorkspaceRole;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec};
    use std::collections::BTreeMap;

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "myapp".into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/myapp".into(),
                    host: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/myapp".into(),
                    host: Some("m2.local".into()),
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
                    role: Default::default(),
                },
                WorkspaceSpec {
                    name: "bob".into(),
                    machine: "m2".into(),
                    runner: "claude".into(),
                    role: Default::default(),
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
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
    fn prompt_includes_task_id_branch_and_review_marker_instruction() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        assert!(prompt.contains(".claude/shelbi-review-ready"));
        assert!(prompt.contains("printf"));
        assert!(!prompt.contains("shelbi task move"));
        assert!(prompt.contains("\n---\n"));
        // A hook-capable runner (claude) gets no polling instructions.
        assert!(!prompt.contains(".shelbi/messages/"));
        assert!(!prompt.contains("message-ack"));
    }

    #[test]
    fn prompt_falls_back_to_task_id_heading_when_body_empty() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt =
            compose_prompt("fix-login", "shelbi/fix-login", "   ", &marker, "main", "myapp", false);
        assert!(prompt.contains("# Task fix-login"));
        assert!(prompt.contains(".claude/shelbi-review-ready"));
    }

    #[test]
    fn polling_runner_prompt_includes_message_log_instructions() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        assert!(prompt.contains(".claude/shelbi-review-ready"));
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
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        assert!(prompt.contains("Resumed."), "missing resume banner: {prompt}");
        assert!(prompt.contains("Fix the Safari SSO bug."), "body dropped: {prompt}");
        assert!(
            prompt.contains("git fetch origin main && git rebase origin/main"),
            "handoff rebase missing: {prompt}"
        );
        assert!(prompt.contains(".claude/shelbi-review-ready"), "marker missing: {prompt}");
        // The banner is a preamble — it precedes the body + handoff.
        let banner_at = prompt.find("Resumed.").unwrap();
        let body_at = prompt.find("Fix the Safari SSO bug.").unwrap();
        assert!(banner_at < body_at, "banner must precede the body: {prompt}");
    }

    #[test]
    fn resume_prompt_banner_wording_depends_on_conversation_resume() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
        // non-claude runner has no such flag shelbi drives, so it's untouched.
        let p = fixture_project();
        let claude = p.runner("claude").unwrap();
        let launch = workspace_launch_command(claude, &p.workspace_permissions_mode, false, true);
        assert!(launch.contains("--continue"), "claude resume must add --continue: {launch}");

        let codex = AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            dialog_signatures: vec![],
        };
        let launch = workspace_launch_command(&codex, &p.workspace_permissions_mode, false, true);
        assert!(
            !launch.contains("--continue"),
            "non-claude runner must not get --continue: {launch}"
        );

        // A normal (non-resume) dispatch never adds --continue, even for claude.
        let launch = workspace_launch_command(claude, &p.workspace_permissions_mode, false, false);
        assert!(!launch.contains("--continue"), "non-resume must not add --continue: {launch}");
    }

    // Captured from a workspace pane that had just submitted its prompt and
    // was mid-turn — used to pin the busy-state heuristic against
    // claude's actual rendered output. The point of this whole helper is
    // that nothing here mentions `shelbi:` anywhere: claude's own OSC 2
    // writes have already clobbered the workspace's `shelbi:working` title
    // marker, so the pane-title probe would have missed this state.
    const BUSY_SCREEN_SPINNER: &str = "\
✻ Brewed for 1m 1s · 2 shells, 1 monitor still running

· Booping… (7m 16s · ↑ 19.8k tokens)
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  Model: Opus 4.7 | Ctx Used: 17.0% | Cost: $4.69
  ⏵⏵ auto mode on (shift+tab to cycle)";

    const BUSY_SCREEN_ESC_FOOTER: &str = "\
⏺ Update(crates/shelbi-orchestrator/src/review.rs)
  ⎿  Added 1 line

✳ Working on the fix...
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  esc to interrupt · ctrl+c twice to exit";

    // The readiness detection (input-box vs trust dialog) moved to
    // `crate::ready`, but the `claude_is_processing` tests below still use
    // these two real captures as negative cases — a live/empty input box
    // and a trust dialog are both NOT the "mid-turn processing" state.
    const TRUST_DIALOG_SCREEN: &str = "\
 Do you trust the files in this folder?

 /work/myapp/.shelbi/wt/bob

 Claude Code may read, edit, and execute files here.

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel";

    const INPUT_BOX_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ Try \"edit <filepath> to...\"
────────────────────────────────────────────────────
  ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents";

    #[test]
    fn claude_is_processing_detects_busy_pane_when_title_marker_lost() {
        // Both fixtures are post-submit screens where claude is mid-turn.
        // Neither has a `shelbi:` title marker (claude's own OSC 2 writes
        // have already overwritten it), so the title-based probe alone
        // would mis-fire a `prompt-lost` abort on a prompt that actually
        // landed. The content fallback catches both.
        assert!(claude_is_processing(BUSY_SCREEN_SPINNER));
        assert!(claude_is_processing(BUSY_SCREEN_ESC_FOOTER));
    }

    #[test]
    fn claude_is_processing_does_not_fire_on_empty_input_or_trust_dialog() {
        // The empty-input ready screen — what the pane looks like
        // BEFORE the prompt is typed. Must not match, otherwise the
        // probe declares success before we've even sent Enter.
        assert!(!claude_is_processing(INPUT_BOX_SCREEN));
        // Trust dialog before claude has accepted the first prompt —
        // the prompt would've been typed INTO this dialog instead of an
        // input box, and we want the probe to keep waiting (and the
        // trust-dismiss path to dismiss it) rather than spuriously
        // signal "submitted."
        assert!(!claude_is_processing(TRUST_DIALOG_SCREEN));
        assert!(!claude_is_processing(""));
        assert!(!claude_is_processing("➜  bob git:(main) claude"));
    }

    #[test]
    fn claude_is_processing_matches_case_insensitively() {
        // Claude's footer text has rendered both "ESC to interrupt" and
        // "esc to interrupt" across versions; we lower-case the screen
        // before matching so neither slips through.
        assert!(claude_is_processing("ESC to interrupt"));
        // The token-counter parenthetical matches in either streaming
        // direction (↑ user-prompt, ↓ tool-output).
        assert!(claude_is_processing("(12s · ↑ 1.2k tokens)"));
        assert!(claude_is_processing("(45s · ↓ 8k tokens)"));
    }

    #[test]
    fn claude_is_processing_does_not_false_positive_on_trust_dialog_footer() {
        // The trust-this-folder dialog footer reads "Enter to confirm ·
        // Esc to cancel" — that "esc to" prefix is the same one claude
        // uses in its busy footer ("esc to interrupt"). We deliberately
        // do NOT include "esc to cancel" in the busy markers because
        // the trust dialog must never read as "claude submitted my
        // prompt and is working" — the prompt was typed INTO the
        // dialog, not into claude's input. Pin that behavior so a
        // future "be more inclusive" tweak can't quietly regress it.
        assert!(!claude_is_processing("Enter to confirm · Esc to cancel"));
    }

    // A pane whose prompt is still sitting UN-submitted in the input box:
    // claude echoed the typed text but Enter never landed, so the box (the
    // region between the last two ──── rules) reproduces the prompt, wrapped
    // across a couple of rows the way claude renders it.
    const STALLED_INPUT_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ # dispatch: enter-stalled false positive — submit
  signal detector reports a stall on submitted prompts
────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";

    fn stalled_prompt() -> String {
        // Contains the two lines the box shows above, contiguously (the box
        // wraps them, but the source prompt has them on one logical line).
        "# dispatch: enter-stalled false positive — submit \
         signal detector reports a stall on submitted prompts\n\n\
         Fix the detector."
            .to_string()
    }

    #[test]
    fn input_holds_prompt_true_when_box_still_shows_prompt() {
        // The genuine-stall case: the prompt is visibly parked in the input
        // box, so we must still be willing to warn.
        assert!(input_holds_prompt(STALLED_INPUT_SCREEN, &stalled_prompt()));
        assert!(!input_box_cleared(STALLED_INPUT_SCREEN, &stalled_prompt()));
    }

    #[test]
    fn input_holds_prompt_false_when_box_empty_or_placeholder() {
        // A submitted prompt leaves the box empty (busy pane) or showing only
        // claude's dim placeholder (idle-after-submit) — neither is the
        // prompt, so no warning. This is the false-positive the fix closes.
        let prompt = stalled_prompt();
        assert!(!input_holds_prompt(BUSY_SCREEN_SPINNER, &prompt));
        assert!(!input_holds_prompt(INPUT_BOX_SCREEN, &prompt));
        // ...and both read as a *cleared* box, our positive submit signal.
        assert!(input_box_cleared(BUSY_SCREEN_SPINNER, &prompt));
        assert!(input_box_cleared(INPUT_BOX_SCREEN, &prompt));
    }

    #[test]
    fn input_box_helpers_handle_missing_box() {
        // No rules on screen (a modal dialog, or a pre-render capture): we
        // can't locate the box, so we neither claim the prompt is stuck nor
        // claim it cleared — both stay false, keeping us from crying wolf.
        assert!(!input_holds_prompt(TRUST_DIALOG_SCREEN, &stalled_prompt()));
        assert!(!input_box_cleared(TRUST_DIALOG_SCREEN, &stalled_prompt()));
        assert!(!input_holds_prompt("", &stalled_prompt()));
        assert!(!input_box_cleared("", &stalled_prompt()));
    }

    #[test]
    fn input_holds_prompt_ignores_short_coincidental_overlap() {
        // A one-line box that only shares a short token with the prompt must
        // not trip the match — that's how the placeholder and unrelated
        // half-typed lines are kept from reading as the dispatched prompt.
        let screen = "\
────────────────────────────────────────────────────
❯ Fix the detector.
────────────────────────────────────────────────────
  ? for shortcuts";
        assert!(!input_holds_prompt(screen, &stalled_prompt()));
    }

    // The exact state the auto-restart bug left the pane in: claude relaunched,
    // the multi-line task prompt was pasted, but the trailing Enter was dropped
    // — so the prompt sits un-submitted, collapsed into a paste chip. Its body
    // is never echoed, so `input_holds_prompt`'s text match sees nothing and
    // (before the fix) the box read as "cleared" → false submit confirmation.
    const PASTED_CHIP_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ [Pasted text #1 +45 lines]
────────────────────────────────────────────────────
  Ctx Used: 0.0% · Cost: $0.00";

    #[test]
    fn is_pasted_chip_matches_collapsed_paste_placeholder() {
        assert!(is_pasted_chip("[Pasted text #1 +45 lines]"));
        assert!(is_pasted_chip("  [Pasted text #12 +3 lines]  "));
        // Not a chip: the dim placeholder, an echoed prompt line, empty.
        assert!(!is_pasted_chip("Try \"edit <filepath> to...\""));
        assert!(!is_pasted_chip("# dispatch: enter-stalled false positive"));
        assert!(!is_pasted_chip(""));
    }

    #[test]
    fn pasted_chip_reads_as_unsubmitted_not_cleared() {
        // The fix: a collapsed paste chip is an UN-submitted prompt. It must
        // NOT read as a cleared box (that was the false submit signal that let
        // the restarted worker sit idle at `❯ [Pasted text #1 +45 lines]`).
        let prompt = stalled_prompt();
        assert!(input_holds_pasted_chip(PASTED_CHIP_SCREEN));
        assert!(input_holds_unsubmitted_prompt(PASTED_CHIP_SCREEN, &prompt));
        assert!(!input_box_cleared(PASTED_CHIP_SCREEN, &prompt));
        // The chip body is never echoed, so the plain text match still misses
        // it — which is exactly why the dedicated chip detector is needed.
        assert!(!input_holds_prompt(PASTED_CHIP_SCREEN, &prompt));
    }

    #[test]
    fn dim_placeholder_is_not_mistaken_for_a_paste_chip() {
        // Regression guard: claude's dim "Try …" placeholder on a genuinely
        // empty box must stay a *cleared* box (a real submit signal). Only the
        // bracketed paste chip flips a box to un-submitted.
        let prompt = stalled_prompt();
        assert!(!input_holds_pasted_chip(INPUT_BOX_SCREEN));
        assert!(!input_holds_unsubmitted_prompt(INPUT_BOX_SCREEN, &prompt));
        assert!(input_box_cleared(INPUT_BOX_SCREEN, &prompt));
        // A busy/mid-turn pane has no chip either — still cleared.
        assert!(!input_holds_pasted_chip(BUSY_SCREEN_SPINNER));
        assert!(input_box_cleared(BUSY_SCREEN_SPINNER, &prompt));
    }

    #[test]
    fn review_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_project();
        let marker = workspace_review_marker(&p.machines[0], &p.workspaces[0]);
        assert_eq!(
            marker,
            PathBuf::from("/tmp/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready")
        );
    }

    #[test]
    fn read_review_marker_returns_valid_task_id_and_rejects_garbage() {
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
        let marker = dir.join("shelbi-review-ready");

        // Absent → None.
        assert!(read_review_marker(&Host::Local, &marker).unwrap().is_none());

        // Valid id, with the trailing newline the worker writes → trimmed id.
        std::fs::write(&marker, "fix-state-runtime-hardening\n").unwrap();
        assert_eq!(
            read_review_marker(&Host::Local, &marker).unwrap().as_deref(),
            Some("fix-state-runtime-hardening")
        );

        // Empty (or whitespace-only) → None, not an error.
        std::fs::write(&marker, "\n").unwrap();
        assert!(read_review_marker(&Host::Local, &marker).unwrap().is_none());

        // A body carrying spaces (a torn write, or an injected value) is not
        // a valid task id → Err, so the caller doesn't clear it.
        std::fs::write(&marker, "fix login now").unwrap();
        assert!(read_review_marker(&Host::Local, &marker).is_err());

        // A multi-line body (e.g. an OSC-injected second record) also fails
        // validation on the first line's stray content.
        std::fs::write(&marker, "evil id\nsecond line").unwrap();
        assert!(read_review_marker(&Host::Local, &marker).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Build a project fixture with two review workspaces on `hub` (plus a dev
    /// slot) so the port / partitioning helpers have something to index into.
    fn fixture_with_review_workspaces() -> Project {
        let mut p = fixture_project();
        p.workspaces = vec![
            WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: WorkspaceRole::Dev,
            },
            WorkspaceSpec {
                name: "review-1".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: WorkspaceRole::Review,
            },
            WorkspaceSpec {
                name: "review-2".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: WorkspaceRole::Review,
            },
        ];
        p
    }

    #[test]
    fn review_workspace_port_is_deterministic_per_index() {
        // Defaults (base 3000, stride 10): review-1 → 3000, review-2 → 3010.
        let p = fixture_with_review_workspaces();
        let r1 = p.workspaces.iter().find(|w| w.name == "review-1").unwrap();
        let r2 = p.workspaces.iter().find(|w| w.name == "review-2").unwrap();
        assert_eq!(review_workspace_port(&p, r1), Some(3000));
        assert_eq!(review_workspace_port(&p, r2), Some(3010));
    }

    #[test]
    fn review_workspace_port_honors_custom_base_and_stride() {
        let mut p = fixture_with_review_workspaces();
        p.review.base_port = 4000;
        p.review.port_stride = 20;
        let r1 = p.workspaces.iter().find(|w| w.name == "review-1").unwrap();
        let r2 = p.workspaces.iter().find(|w| w.name == "review-2").unwrap();
        assert_eq!(review_workspace_port(&p, r1), Some(4000));
        assert_eq!(review_workspace_port(&p, r2), Some(4020));
    }

    #[test]
    fn review_workspace_port_is_none_for_a_dev_workspace() {
        // A dev slot must never be handed a review port.
        let p = fixture_with_review_workspaces();
        let dev = p.workspaces.iter().find(|w| w.name == "alpha").unwrap();
        assert_eq!(review_workspace_port(&p, dev), None);
    }

    #[test]
    fn loaded_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_with_review_workspaces();
        let r1 = p.workspaces.iter().find(|w| w.name == "review-1").unwrap();
        let marker = workspace_review_loaded_marker(&p.machines[0], r1);
        assert_eq!(
            marker,
            PathBuf::from("/tmp/myapp/.shelbi/wt/review-1/.claude/shelbi-review-loaded")
        );
    }

    #[test]
    fn read_review_loaded_marker_parses_task_and_url() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-loaded-marker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("shelbi-review-loaded");

        // Absent → None.
        assert!(read_review_loaded_marker(&Host::Local, &marker).unwrap().is_none());

        // task id + URL → both parsed (URL trimmed of the trailing newline).
        std::fs::write(&marker, "fix-login http://alpha.local:3000\n").unwrap();
        assert_eq!(
            read_review_loaded_marker(&Host::Local, &marker).unwrap(),
            Some(ReviewLoaded {
                task_id: "fix-login".into(),
                url: Some("http://alpha.local:3000".into()),
            })
        );

        // Bare task id (diff-only review — nothing runnable) → url None.
        std::fs::write(&marker, "fix-login\n").unwrap();
        assert_eq!(
            read_review_loaded_marker(&Host::Local, &marker).unwrap(),
            Some(ReviewLoaded { task_id: "fix-login".into(), url: None })
        );

        // Trailing whitespace with no URL is still a diff-only load.
        std::fs::write(&marker, "fix-login   \n").unwrap();
        assert_eq!(
            read_review_loaded_marker(&Host::Local, &marker).unwrap(),
            Some(ReviewLoaded { task_id: "fix-login".into(), url: None })
        );

        // Empty → None, not an error.
        std::fs::write(&marker, "\n").unwrap();
        assert!(read_review_loaded_marker(&Host::Local, &marker).unwrap().is_none());

        // A hostile task-id token fails validation → Err (poller leaves it).
        std::fs::write(&marker, "../etc/passwd http://x").unwrap();
        assert!(read_review_loaded_marker(&Host::Local, &marker).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transition_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_with_review_workspaces();
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
        assert!(read_transition_marker(&Host::Local, &marker).unwrap().is_none());

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
        assert!(read_transition_marker(&Host::Local, &marker).unwrap().is_none());

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
    fn review_load_prompt_carries_context_port_and_loaded_marker_contract() {
        let marker =
            PathBuf::from("/tmp/myapp/.shelbi/wt/review-1/.claude/shelbi-review-loaded");
        let prompt = compose_review_load_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            Some(3000),
        );
        // Task context + branch.
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("shelbi/fix-login"));
        // The assigned PORT is surfaced to the agent.
        assert!(prompt.contains("3000"));
        assert!(prompt.contains("$PORT"));
        // The loaded-marker path + atomic write (temp + mv), carrying the URL.
        assert!(prompt.contains(".claude/shelbi-review-loaded"));
        assert!(prompt.contains("shelbi-review-loaded.tmp"));
        assert!(prompt.contains("printf '%s %s"));
        assert!(prompt.contains("\"$URL\""));
        assert!(prompt.contains("review_loaded"));
        // A review load explicitly does NOT rebase (that's the dev handoff).
        assert!(!prompt.contains("git rebase"));
        // And it tells the agent not to modify code.
        assert!(prompt.contains("do NOT modify code"));
    }

    #[test]
    fn review_load_prompt_omits_port_line_when_no_port() {
        let marker =
            PathBuf::from("/tmp/myapp/.shelbi/wt/review-1/.claude/shelbi-review-loaded");
        let prompt = compose_review_load_prompt(
            "fix-login",
            "shelbi/fix-login",
            "context",
            &marker,
            None,
        );
        // No port → no "assigned dev-server port" paragraph, but the marker
        // contract is still delivered (a diff-only review still hands off).
        assert!(!prompt.contains("assigned dev-server port"));
        assert!(prompt.contains(".claude/shelbi-review-loaded"));
    }

    #[test]
    fn prompt_writes_marker_atomically_via_tmp_and_mv() {
        // The worker must write the marker to a sibling temp file and `mv`
        // it into place — a rename within one directory is atomic, so the
        // poller never `cat`s a half-written body (which would fail
        // `validate_task_id` and stall the handoff).
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
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
                 /work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready.tmp && \
                 mv /work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready.tmp \
                 /work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready"
            ),
            "marker write is not atomic (tmp + mv): {prompt}"
        );
    }

    #[test]
    fn parses_typical_claude_version_output() {
        assert_eq!(parse_claude_version("2.1.83 (Claude Code)\n"), Some((2, 1, 83)));
        assert_eq!(parse_claude_version("2.1.153 (Claude Code)"), Some((2, 1, 153)));
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
        let runner = AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] };
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
                dialog_signatures: vec![],
            },
        );
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
    }

    #[test]
    fn spawn_path_leaves_non_claude_runners_alone() {
        // Codex doesn't understand --permission-mode; injecting it would
        // crash the runner on launch.
        let mut p = fixture_project();
        p.agent_runners.insert(
            "codex".into(),
            AgentRunnerSpec { command: "codex".into(), flags: vec!["--print".into()], dialog_signatures: vec![] },
        );
        let runner = p.runner("codex").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "codex --print");
    }

    #[test]
    fn both_host_kinds_construct_the_same_launch_command() {
        // F12: the local dispatch path (via the `shelbi open --as-pane`
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
        let runner = AgentRunnerSpec { command: "codex".into(), flags: vec!["--print".into()], dialog_signatures: vec![] };
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
        let line = remote_cd_launch(&wt, "claude --permission-mode auto", None, "bob");
        let expected = shelbi_state::remote_hub_socket_path();
        assert!(
            line.contains(&format!("SHELBI_HUB_SOCK={}", expected.display())),
            "expected default socket path in: {line}"
        );
        // No PORT on the dev path.
        assert!(!line.contains("PORT="), "dev path must not inject PORT: {line}");
        // Must scope to the exec'd shell, not the surrounding tmux
        // pane — otherwise the `$SHELL -lc` env-strip drops it before
        // claude inherits it.
        let env_at = line.find("SHELBI_HUB_SOCK=").unwrap();
        let exec_at = line.find("exec ").unwrap();
        assert!(env_at < exec_at, "SHELBI_HUB_SOCK must come BEFORE exec: {line}");
        assert!(line.starts_with("cd "), "still cd's into the worktree: {line}");
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
        let line = remote_cd_launch(&wt, "claude", None, "bob");
        assert!(
            line.contains("SHELBI_HUB_SOCK=/run/user/1000/shelbi.sock"),
            "override not honored: {line}"
        );
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
    }

    /// A review dispatch injects the deterministic `PORT` into the remote
    /// pane's exec env, scoped before `exec` (so the `$SHELL -lc` env-strip
    /// doesn't drop it), alongside the hub socket.
    #[test]
    fn remote_cd_launch_injects_port_for_review_workspace() {
        let _g = crate::test_lock::acquire();
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/review-1");
        let line = remote_cd_launch(&wt, "claude", Some(3010), "review-1");
        assert!(line.contains("PORT=3010"), "expected PORT in: {line}");
        let port_at = line.find("PORT=3010").unwrap();
        let exec_at = line.find("exec ").unwrap();
        assert!(port_at < exec_at, "PORT must come BEFORE exec: {line}");
    }

    #[test]
    fn with_agent_system_prompt_appends_claude_flag_when_agent_set() {
        let runner = AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] };
        let launch = "claude --permission-mode auto";
        let out = with_agent_system_prompt(
            launch,
            Some(WORKTREE_AGENT_INSTRUCTIONS_REL),
            &runner,
        );
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
        assert!(out.starts_with("claude --permission-mode auto"), "got: {out}");
    }

    #[test]
    fn with_agent_system_prompt_noop_when_no_agent_or_non_claude() {
        let claude = AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] };
        let codex = AgentRunnerSpec { command: "codex".into(), flags: vec![], dialog_signatures: vec![] };
        let base = "claude --permission-mode auto";

        // No agent → no flag injection (e.g. a test or non-CLI caller
        // that omits the agent context).
        assert_eq!(
            with_agent_system_prompt(base, None, &claude),
            base,
        );

        // Codex doesn't understand the flag; injecting would crash the
        // runner. The agent's instructions.md is still on disk for any
        // future runner-specific loader to pick up.
        assert_eq!(
            with_agent_system_prompt(
                "codex",
                Some(WORKTREE_AGENT_INSTRUCTIONS_REL),
                &codex,
            ),
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
        let rendered_fallback =
            render_workspace_settings_preferring_agent(&project, None).unwrap();
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
        let out = splice_orchestrator_handoff(
            "# orchestrator\nbody\n",
            "in-flight: nothing\n",
        );
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
                let parent_of_head = String::from_utf8_lossy(
                    &run_git_in(&repo, &["rev-parse", "HEAD~1"]).stdout,
                )
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
            RebaseOutcome::Conflict { stderr_excerpt, files, .. } => {
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

        let outcome =
            rebase_workspace_branch_onto_default(&Host::Local, &repo, "ghost-branch-does-not-exist");
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
        std::fs::write(repo.join(".claude/shelbi-review-ready"), "task-id\n").unwrap();

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
        assert_eq!(user_dirty_porcelain_lines(lookalike), vec!["?? .shelbimeta"]);

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
            argv.iter().any(|s| s == "SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock"),
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
        assert!(argv.iter().any(|s| s == "shelbi-demo:"));
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
        assert_eq!(argv[port_at - 1], "-e", "PORT payload not preceded by -e: {argv:?}");
        let sh_at = argv.iter().position(|s| s == "sh").unwrap();
        assert!(port_at < sh_at, "PORT must precede the sh -c positional: {argv:?}");
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
        let host = Host::Ssh { host: "devbox".into() };
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
        assert!(!skills.exists(), "intended skills dir not removed — wire: {wire}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod sync_worktree_git_tests {
    //! Real-git tests for [`sync_worktree`]'s recovery paths (F5 partial
    //! worktree, F14 branch-checked-out-elsewhere). Each provisions a tiny
    //! on-disk repo whose `work_dir` doubles as the main clone, then drives
    //! `sync_worktree` against `Host::Local`. Skipped when `git` isn't on
    //! PATH so a git-less sandbox still passes.
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, MachineKind, OrchestratorSpec, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

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
        assert!(run_git_in(&dir, &["init", "-q", "-b", "main"]).status.success());
        std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
        assert!(run_git_in(&dir, &["add", "README.md"]).status.success());
        assert!(run_git_in(&dir, &["commit", "-q", "-m", "initial"]).status.success());
        dir
    }

    /// Project with two hub-local workspaces (`alice`, `bob`) whose worktrees
    /// live under `<repo>/.shelbi/wt/`. `work_dir` is the repo itself.
    fn project_at(repo: &std::path::Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] },
        );
        Project {
            name: "sync-test".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec { name: "alice".into(), machine: "hub".into(), runner: "claude".into(), role: Default::default() },
                WorkspaceSpec { name: "bob".into(), machine: "hub".into(), runner: "claude".into(), role: Default::default() },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
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
        // F5: dir exists without a valid `.git` (dispatch killed mid-add).
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
    fn releases_branch_checked_out_in_another_worktree() {
        // F14: the requested branch is already checked out in `alice`'s
        // worktree. Dispatching it to `bob` must detach it from `alice`
        // first instead of dying on "already checked out".
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("release");
        let project = project_at(&repo);
        let machine = project.machines[0].clone();
        assert!(run_git_in(&repo, &["branch", "shelbi/x", "main"]).status.success());

        // alice takes shelbi/x first.
        let alice_wt = workspace_worktree(&machine, &project.workspaces[0]);
        sync_worktree(&project, &Host::Local, &machine, &alice_wt, "shelbi/x", "main").unwrap();
        assert_eq!(head_of(&alice_wt), "shelbi/x");

        // bob is created on its own branch, then re-dispatched onto shelbi/x
        // (which is still live in alice's worktree).
        let bob_wt = workspace_worktree(&machine, &project.workspaces[1]);
        sync_worktree(&project, &Host::Local, &machine, &bob_wt, "shelbi/bobinit", "main").unwrap();
        sync_worktree(&project, &Host::Local, &machine, &bob_wt, "shelbi/x", "main").unwrap();

        assert_eq!(head_of(&bob_wt), "shelbi/x", "bob must claim the branch");
        // alice was released (detached) so the branch was free to move.
        assert_ne!(head_of(&alice_wt), "shelbi/x", "alice must have been detached");
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
        assert!(err.is_err(), "user-authored change must still block the switch");
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
        assert!(run_git_in(&wt, &["commit", "-q", "-m", "wip"]).status.success());
        let committed_sha = String::from_utf8_lossy(
            &run_git_in(&wt, &["rev-parse", "HEAD"]).stdout,
        )
        .trim()
        .to_string();
        // Uncommitted change that a `start`-style clean checkout would reject.
        std::fs::write(wt.join("scratch.txt"), "half-finished\n").unwrap();

        // Resume-sync must be a no-op against the tree.
        sync_worktree_for_resume(&Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert_eq!(head_of(&wt), "shelbi/x", "branch must be preserved");
        assert_eq!(
            String::from_utf8_lossy(&run_git_in(&wt, &["rev-parse", "HEAD"]).stdout)
                .trim(),
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
        assert!(run_git_in(&wt, &["commit", "-q", "-m", "landed"]).status.success());
        assert!(run_git_in(&repo, &["worktree", "remove", "--force", &wt.to_string_lossy()])
            .status
            .success());
        assert!(!wt.join(".git").exists(), "worktree should be gone");

        sync_worktree_for_resume(&Host::Local, &machine, &wt, "shelbi/x", "main").unwrap();

        assert!(wt.join(".git").exists(), "worktree must be recreated");
        assert_eq!(head_of(&wt), "shelbi/x", "must reclaim the existing branch");
        assert!(wt.join("work.txt").exists(), "prior commit's file must be present");
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
        assert_git_ok(&run_git_in(repo, &["commit", "-q", "-m", message]), "git commit");
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
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "synccut".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
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
            review: Default::default(),
            contextstore_sync: Vec::new(),
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
        assert_git_ok(&run_git_in(&seed, &["init", "-q", "-b", "main"]), "git init");
        commit_file(&seed, "README.md", "# repo\n", "initial");

        let bare = root.join("origin.git");
        assert_git_ok(
            &run_git_in(
                &root,
                &["clone", "-q", "--bare", seed.to_str().unwrap(), bare.to_str().unwrap()],
            ),
            "bare clone",
        );

        let machine = root.join("machine");
        assert_git_ok(
            &run_git_in(
                &root,
                &["clone", "-q", bare.to_str().unwrap(), machine.to_str().unwrap()],
            ),
            "machine clone",
        );
        let stale = rev_parse(&machine, "main");

        // Advance origin's main behind the machine clone's back.
        let writer = root.join("writer");
        assert_git_ok(
            &run_git_in(
                &root,
                &["clone", "-q", bare.to_str().unwrap(), writer.to_str().unwrap()],
            ),
            "writer clone",
        );
        commit_file(&writer, "upstream.txt", "merged upstream\n", "upstream landed");
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

        sync_worktree(&project_at(&machine), &Host::Local, &machine_at(&machine), &wt, "shelbi/task-x", "main")
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
            !run_git_in(&wt, &["rev-parse", "--abbrev-ref", "shelbi/task-x@{upstream}"])
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
                &["worktree", "add", "-q", "-b", "shelbi/prev-task", wt.to_str().unwrap(), "main"],
            ),
            "pre-existing worktree",
        );

        sync_worktree(&project_at(&machine), &Host::Local, &machine_at(&machine), &wt, "shelbi/task-y", "main")
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
                &["remote", "set-url", "origin", "/nonexistent/shelbi-gone.git"],
            ),
            "break origin",
        );
        let wt = root.join("wt-charlie");

        let err = sync_worktree(&project_at(&machine), &Host::Local, &machine_at(&machine), &wt, "shelbi/task-z", "main")
            .expect_err("unreachable origin must abort the sync");
        let msg = err.to_string();
        assert!(msg.contains("fetch"), "error must name the failing fetch: {msg}");
        assert!(msg.contains("main"), "error must name the base branch: {msg}");
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
        assert_git_ok(&run_git_in(&repo, &["init", "-q", "-b", "main"]), "git init");
        commit_file(&repo, "README.md", "# repo\n", "initial");
        let local_main = rev_parse(&repo, "main");
        let wt = root.join("wt-delta");

        sync_worktree(&project_at(&repo), &Host::Local, &machine_at(&repo), &wt, "shelbi/task-l", "main")
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
                &["remote", "set-url", "origin", "/nonexistent/shelbi-gone.git"],
            ),
            "break origin",
        );
        let wt = root.join("wt-echo");

        sync_worktree(&project_at(&machine), &Host::Local, &machine_at(&machine), &wt, "shelbi/pre-cut", "main")
            .expect("existing branch must check out without fetching");
        assert_eq!(rev_parse(&machine, "shelbi/pre-cut"), stale);
        let out = run_git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_git_ok(&out, "worktree HEAD");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi/pre-cut");

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
                &["remote", "set-url", "origin", "/nonexistent/shelbi-gone.git"],
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
                dialog_signatures: vec![],
            },
        );
        let project = Project {
            name: "synccut".into(),
            repo: machine_repo.to_string_lossy().into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: machine_repo.clone(),
                host: None,
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
                role: Default::default(),
            }],
            workspace_poll_interval_secs: 5,
            // NOT "auto": keeps `require_auto_mode_supported` from probing
            // the host's claude binary — this test is about the sync step.
            workspace_permissions_mode: "acceptEdits".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
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

        assert!(result.is_err(), "start must abort when the sync fetch fails");
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
