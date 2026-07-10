# Adversarial review: Zen auto-merge safety (orchestrator zen/actions/git)

Reviewed:

- `crates/shelbi-orchestrator/src/zen.rs` (2733 lines)
- `crates/shelbi-orchestrator/src/actions.rs` (2549 lines)
- `crates/shelbi-orchestrator/src/git.rs` (372 lines)

Supporting code read for context (not in scope, not reported against):
`crates/shelbi-orchestrator/src/workspace.rs` (`rebase_workspace_branch_onto_default`),
`crates/shelbi-agent/src/lib.rs` (`shell_escape`),
`crates/shelbi-ssh/src/lib.rs` (`run`/`build_command`),
`crates/shelbi-core/src/model.rs` (`validate_task_id`, `MergeStrategy`),
`crates/shelbi-state/src/lib.rs` (`save_task`).

All git/diff behaviors below were confirmed against a scratch git repo, not
inferred from memory.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | No head-SHA pin between probe/ci-watch and `pr_merge` — TOCTOU lets an unchecked commit merge | high | certain | assumption |
| F2 | Concurrent hub-side merges mutate one shared `work_dir` checkout with no lock | high | certain | failure-scenario |
| F3 | Diff-size and danger-path scans use two-dot `base..branch` against a possibly-stale local base; renames hide the old danger path | high | certain | bug |
| F4 | Binary blobs and pure renames evade the diff-size line limit (shortstat reports 0 lines) | medium | certain | assumption |
| F5 | `task.branch` frontmatter is never validated; it flows unguarded into git argv (no `--` separators) | medium | certain | hardening |
| F6 | A repo with zero CI checks never goes green — `ci_watch` reports `red:unknown` forever | medium | certain | bug |
| F7 | `fetch_probe_base` silently falls back to a stale local base on any fetch failure | medium | likely | failure-scenario |
| F8 | `merge_via_pr` / `pr_merge` read `mergeCommit.oid` immediately and hard-error on empty — async GitHub merges race it | medium | likely | failure-scenario |
| F9 | `merge()` ViaPr path restacks children onto the project base, not the PR's real base | medium | likely | bug |
| F10 | A child depending on two concurrently-merging parents hits a force-with-lease race and lands on only one base | medium | speculative | failure-scenario |
| F11 | `merge_hub_side` leaves the hub work_dir checked out on `target`, not restored | low | certain | best-practice |
| F12 | `lookup_open_pr` silently picks `.[0]` when a branch has multiple open PRs | low | likely | assumption |
| F13 | Danger-path globs are case-sensitive; a case-variant path evades them | low | likely | hardening |

---

## F1: No head-SHA pin between probe/ci-watch and `pr_merge` — TOCTOU
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:266` (`pr_merge`), sequenced after `ci_watch` (`zen.rs:195`) and `probe_in_workflow` (`zen.rs:547`)
- **Category:** assumption (atomicity of "checks passed" → "merge")
- **Severity / Confidence:** high / certain
- **Evidence:** The three Zen primitives are independent CLI invocations sequenced by the orchestrator's prompt (module doc, `zen.rs:1-6`: "The orchestrator sequences them per its Merge Conditions policy; no primitive implies what the next should do"). `pr_merge` issues:

  ```rust
  let out = run_in_dir(
      &host,
      &wt,
      &["gh", "pr", "merge", &pr_str, strategy_flag, "--delete-branch"],
  )?;
  ```

  There is no `--match-head-commit <oid>` and no comparison of the PR's current head against the SHA the probe/ci-watch actually evaluated. `gh pr merge <pr>` merges whatever the PR's head is *now*.
- **Failure scenario:** `probe` passes and `ci_watch` returns `Green` for PR #42 at commit `A`. Before the orchestrator fires `pr_merge`, the workspace (or a human) pushes commit `B` to the same branch. `pr_merge` merges `B` — code that was never probed and whose CI may still be pending or failing. The `--delete-branch` then removes the branch, erasing the un-reviewed head. This is the classic auto-merge TOCTOU and the exact case seed question #1 asks about.
- **Recommendation:** Capture the head SHA at probe/ci-watch time and pass `gh pr merge --match-head-commit <sha>` (gh supports this); merge fails loudly if the head moved, and the orchestrator re-probes. At minimum, re-read `gh pr view --json headRefOid` immediately before merge and refuse if it differs from the probed SHA.
- **Effort:** M

## F2: Concurrent hub-side merges mutate one shared checkout with no lock
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:825` (`merge_hub_side`), reached from `merge` (`actions.rs:373`); same shared `work_dir` returned by `locate_hub_workdir` (`git.rs:110`)
- **Category:** failure-scenario (concurrent invocation)
- **Severity / Confidence:** high / certain
- **Evidence:** `merge_hub_side` runs a stateful mutation sequence directly in the hub's single `work_dir`:

  ```rust
  run_or_command_err(host, wt, &["git", "checkout", target], ...)?;
  run_or_command_err(host, wt, &["git", "merge", "--ff-only", &format!("origin/{target}")], ...)?;
  // ... git merge --squash / --no-ff ...
  run_or_command_err(host, wt, &["git", "commit", "-m", &msg], ...)?;
  run_or_command_err(host, wt, &["git", "push", "origin", target], ...)?;
  ```

  `wt` here is the hub `work_dir` (`merge` binds `let (host, dir) = locate_hub_workdir(project)?; let wt = dir.to_string_lossy()...`, `actions.rs:384-385`). A grep of `actions.rs`/`zen.rs` finds no `Mutex`/`flock`/lockfile guarding this region — the only locks present are `#[cfg(test)]` (`auto_fire_lock`, `actions.rs:2401`). `restack` was deliberately given per-call temp worktrees (`actions.rs:607-628`) to avoid exactly this class of collision, but `merge_hub_side` was not.
- **Failure scenario:** Two tasks reach merge at once (two workspaces, Zen loop iterating). Merge A runs `git checkout main`; merge B runs `git checkout main` concurrently; their index/worktree operations interleave, producing "index.lock exists", a merge commit that includes B's staged squash on top of A, or a `push` that fast-forwards the wrong tip. Worst case B's `git merge --squash` stages into A's in-progress index and both get committed together. Seed question #6.
- **Recommendation:** Serialize hub-side git mutations behind a project-scoped lock (advisory lockfile in `.shelbi/`), or give `merge_hub_side` its own detached temp worktree the way `restack` already does.
- **Effort:** M

## F3: Diff-size and danger-path scans use two-dot `base..branch` against a stale base; renames hide the old danger path
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:924` (`probe_diff_size`), `zen.rs:982` (`probe_danger_paths`)
- **Category:** bug (wrong diff range + rename blindness)
- **Severity / Confidence:** high / certain
- **Evidence:** Both build a two-dot range and diff the endpoints:

  ```rust
  let range = format!("{main}..{branch}");        // probe_diff_size
  ...
  let range = format!("{base}..{branch}");        // probe_danger_paths
  let stdout = shelbi_ssh::run_capture(host,
      ["git", "-C", wt.as_str(), "diff", "--name-only", range.as_str()])?;
  ```

  For `git diff`, `A..B` compares the *tips* of A and B (identical to `git diff A B`), **not** the merge-base three-dot `A...B`. Under `RebasePolicy::AsIs` (the legacy `probe` entry and the read-only `dry_run_tick`, `zen.rs:2349`), `base`/`main` is the worktree's local default ref, which can be behind the branch's real fork point. Verified in a scratch repo: after advancing `main` past the fork point, `git diff --name-only main..feature` listed `keep.txt` — a file **only `main` changed** — as part of the branch's diff, because divergent upstream commits show up reversed. This inflates `DiffSize` (can trip `diff-too-large`) and injects paths the branch never touched into the danger scan.

  Separately, `--name-only` with default rename detection reports only the *destination* path. Verified: renaming `.github/workflows/ci.yml` → `renamed-ci.yml` (R100) produced only `renamed-ci.yml` in `--name-only`. So a rename *out of* a danger glob (`.github/workflows/**`) is invisible to `match_danger_paths` — the old danger path never appears, and the new name doesn't match the glob either. A branch can rewrite or move a protected CI/workflow/lockfile and evade the danger gate. Seed questions #2 and #3.
- **Failure scenario:** A branch does `git mv .github/workflows/deploy.yml deploy.yml` plus edits. `probe_danger_paths` sees only `deploy.yml`, no danger match, `evaluate_probe` returns `Merge`, and a CI-pipeline change auto-merges without the danger warning that exists to catch exactly this.
- **Recommendation:** Use three-dot `base...branch` (merge-base relative) for both scans so upstream divergence doesn't pollute the branch diff, and add `--find-renames` handling: run `git diff --name-status -M base...branch` and feed *both* old and new paths of an `R` row to `match_danger_paths`. Consider `--no-renames` for the danger scan specifically so the pre-rename path is always surfaced.
- **Effort:** M

## F4: Binary blobs and pure renames evade the diff-size line limit
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:948` (`parse_shortstat`), consumed by `evaluate_probe` at `zen.rs:2454`
- **Category:** assumption ("diff size == line count")
- **Severity / Confidence:** medium / certain
- **Evidence:** `evaluate_probe` gates on `files` and `lines_added + lines_removed`:

  ```rust
  let total_lines = report.diff_size.lines_added + report.diff_size.lines_removed;
  if report.diff_size.files > DRYRUN_MAX_DIFF_FILES || total_lines > DRYRUN_MAX_DIFF_LINES {
  ```

  `git diff --shortstat` reports **0 insertions / 0 deletions for binary files**. Verified: a 100 KB random binary produced ` 1 file changed, 0 insertions(+), 0 deletions(-)`. Pure renames likewise contribute no line churn. `parse_shortstat` faithfully records `lines_added = lines_removed = 0`.
- **Failure scenario:** A branch commits a single 500 MB binary asset (or a vendored blob). `files = 1`, `total_lines = 0` — both under the `30 files / 2000 lines` bar, so `evaluate_probe` returns `Merge`. The size gate that exists to keep auto-merge to small, reviewable diffs is bypassed by exactly the kind of change (large binaries) that most warrants human eyes.
- **Recommendation:** Add a byte-size signal (`git diff --stat` includes `Bin … bytes` lines, or `git diff --numstat` marks binaries with `-`/`-`) and gate on total bytes and/or "any binary file changed" in addition to line count.
- **Effort:** M

## F5: `task.branch` is never validated; it flows unguarded into git argv
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:1084` (`require_branch`), used at `actions.rs:188`, `1050`, `1068`, `661`, `669`; `crates/shelbi-state/src/lib.rs:1106` (`save_task`)
- **Category:** hardening (argument injection / malformed refspec)
- **Severity / Confidence:** medium / certain
- **Evidence:** `save_task` performs no validation of the `branch:` frontmatter field (it only resolves the path from the validated `task.id`), and `Task::branch` is a free-form `Option<String>` (`model.rs:834`). `require_branch` returns it verbatim, and it is spliced positionally into git commands with no `--` end-of-options guard:

  ```rust
  run_in_dir(&host, &wt, &["git", "push", "-u", "origin", &branch])?;          // push_branch
  run_in_dir(&host, &wt, &["git", "push", "origin", "--delete", &branch])?;    // delete_branch
  &format!("--force-with-lease={child_branch}"),                                // restack
  &format!("HEAD:{child_branch}"),
  ```

  Shell injection is *not* the risk — `run_in_dir` routes through `shell_escape`. The risk is git **argument injection**: a `branch:` value beginning with `-` (e.g. `--exec=…` style or any dashed token) is parsed by git as an option because there is no `--` before the ref operand. Values containing `:` also form surprising refspecs in the `HEAD:{child_branch}` push.
- **Failure scenario:** An out-of-band edit (or a future feature that derives `branch` from user text) sets `branch: --delete`. `git push -u origin --delete` is misparsed; more hostile dashed values could select unintended git behavior. Even benign-but-weird names (`feat/x:y`) corrupt the `HEAD:{child_branch}` refspec in `restack`.
- **Recommendation:** Validate `branch` on `save_task` with a git-ref-safe character set (reuse the `validate_task_id` alphabet plus `/`), and insert `--` before ref operands in the push/delete argv (`git push origin --delete -- <branch>`).
- **Effort:** S

## F6: A repo with zero CI checks never goes green
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:206` (`ci_watch` loop), `zen.rs:98` (`is_no_required_checks_message`)
- **Category:** bug (edge case: no checks configured)
- **Severity / Confidence:** medium / certain
- **Evidence:** In `WatchMode::Required`, a "no required checks reported" message flips to `AllReported` (`zen.rs:224-233`). But in `AllReported`, `gh pr checks <pr>` on a PR with **no checks at all** exits non-zero with `no checks reported on the '<branch>' branch` — a different string. `is_no_required_checks_message` matches only the needle `"no required checks reported"`, and the test `does_not_confuse_real_failures_with_no_required_checks` (`zen.rs:423-431`) explicitly confirms `"no checks reported…"` is **not** matched. So control falls to:

  ```rust
  let (check, summary) = first_failing_check(&stdout).unwrap_or_else(|| {
      let fallback = stdout.lines().last()... ;
      ("unknown".to_string(), fallback)
  });
  return Ok(CiVerdict::Red { check, summary });
  ```

  `first_failing_check` finds nothing (no rows), so the verdict is `Red { check: "unknown", … }`.
- **Failure scenario:** A project with no GitHub Actions/checks (common for small repos, docs sites) runs Zen. `ci_watch` immediately returns `red:unknown:no checks reported…`, and the orchestrator blocks every merge. Seed question #7: 0 checks is treated as **red**, not green — the opposite hazard from the one asked about, but still a hard functional block with a confusing reason.
- **Recommendation:** Detect the "no checks reported on the '…' branch" message distinctly and return an explicit verdict (e.g. `Green` with a "no checks configured" note, or a dedicated `NoChecks` variant the prompt can policy on) rather than `Red{unknown}`.
- **Effort:** S

## F7: `fetch_probe_base` silently falls back to a stale local base on fetch failure
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:616` (`fetch_probe_base`)
- **Category:** failure-scenario (dropped network / offline host)
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  match shelbi_ssh::run(host, ["git", "-C", wt.as_str(), "fetch", "origin", base]) {
      Ok(o) if o.status.success() => format!("origin/{base}"),
      _ => base.to_string(),
  }
  ```

  Any fetch failure — offline host, dropped SSH, rate-limit, transient DNS — collapses to the local `base` ref with no signal in `ProbeReport`. Under `RebasePolicy::RebaseOntoDefault` the subsequent `rebase_workspace_branch_onto_default` then rebases onto (and the conflict/diff/danger facts compare against) a base that may be many commits stale.
- **Failure scenario:** A blocker fix merged to `origin/main` after handoff (the exact case the rebase policy exists for, per `zen.rs:508-513`). A momentary network blip makes the fetch fail; the probe compares against the pre-fix local `main`, reports no conflict and a clean diff, and the orchestrator merges a branch that actually conflicts with current `main`. The real `merge_hub_side` fetches fresh so the *integration* is safe, but the *decision* was made on stale facts.
- **Recommendation:** Surface fetch failure in the report (a `base_fetch_failed` flag or a `Skipped`-style signal) so the orchestrator can decline to auto-merge on stale data instead of treating a degraded probe as authoritative.
- **Effort:** S

## F8: `merge_via_pr` / `pr_merge` hard-error when GitHub hasn't populated `mergeCommit.oid` yet
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:779` (`merge_via_pr`), same shape in `crates/shelbi-orchestrator/src/zen.rs:286` (`pr_merge`)
- **Category:** failure-scenario (async GitHub state)
- **Severity / Confidence:** medium / likely
- **Evidence:** Immediately after `gh pr merge` returns success, the code reads the merge SHA and errors if empty:

  ```rust
  let sha = String::from_utf8_lossy(&view.stdout).trim().to_string();
  if sha.is_empty() {
      return Err(Error::Other(format!(
          "gh pr view {pr_str}: merge reported success but mergeCommit.oid is empty"
      )));
  }
  ```

  GitHub processes a merge asynchronously; `gh pr merge` can return before `mergeCommit` is queryable (notably with merge queues, or squash on a busy repo). There is no retry/poll — a single empty read is fatal.
- **Failure scenario:** The merge *succeeds* on GitHub, but the follow-up `gh pr view` races ahead and sees `mergeCommit: null`. The action returns `Err`, the orchestrator records a failed merge, and a retry then finds the PR already merged/closed — a confusing partial-state (merged branch, "failed" action, no recorded SHA). Related to seed question #4 (partial states).
- **Recommendation:** Poll `gh pr view --json mergeCommit,state` a few times with a short backoff, treating `state == MERGED` with a still-null oid as "merged, SHA pending" rather than a hard error.
- **Effort:** S

## F9: `merge()` ViaPr path restacks children onto the project base, not the PR's real base
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:398` (`merged_target` computation in `merge`)
- **Category:** bug (stacked-workflow base mismatch)
- **Severity / Confidence:** medium / likely
- **Evidence:**

  ```rust
  let merged_target = match &outcome {
      MergeOutcome::HubSide { target, .. } => target.clone(),
      MergeOutcome::ViaPr { .. } => target.clone(),   // = target_override or project.base_branch()
  };
  let restacks = restack_children(project, project_name, task, &branch, &merged_target);
  ```

  On the ViaPr path the actual base is the PR's stored base (chosen by `open_pr`'s resolution chain, `actions.rs:296-329`, which can be a *parent task's branch* in a stacked workflow). But `merged_target` is recomputed from `target_override`/`project.base_branch()`, ignoring where the PR truly merged. The code comment even acknowledges the assumption ("gh pr merge respects the PR's stored base … Mirror the same fallback chain here") — but the mirror is only correct when `open_pr` and `merge` were handed the *same* `target_override`, which the workflow engine does not guarantee across two separate action invocations.
- **Failure scenario:** A stacked PR whose base is `shelbi/parent` is merged with `merge(..., target_override=None)`. `merged_target` becomes `main`, and every child is rebased `--onto main` even though the parent chain actually integrated into `shelbi/parent`. Children land on the wrong base.
- **Recommendation:** On the ViaPr path, read the PR's real base back (`gh pr view --json baseRefName`) and pass *that* as `onto` to `restack_children`, instead of re-deriving it.
- **Effort:** S

## F10: A child of two concurrently-merging parents hits a force-with-lease race
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:421` (`restack_children`), `actions.rs:661` (force-with-lease push in `restack`)
- **Category:** failure-scenario (concurrent invocation)
- **Severity / Confidence:** medium / speculative
- **Evidence:** `merge` auto-fires `restack_children` for every not-`Done` task listing the merged task in `depends_on`. A child with two parents (`depends_on: [p1, p2]`) is a legal shape. If `p1` and `p2` merge concurrently (see F2), both merges enumerate the same child and both call `restack` on the same `child_branch`, racing on:

  ```rust
  &format!("--force-with-lease={child_branch}"),
  ```

  The lease uses the just-fetched `origin/<child>`; whichever push lands second fails the lease and is caught as `Err` → `RestackOutcome::Skipped { reason: "restack-error:…" }` (`actions.rs:453-456`).
- **Failure scenario:** Child ends up rebased onto only the first parent's target; the second restack is silently downgraded to a `Skipped` line the operator may not notice, leaving the child's base inconsistent with one of its merged parents.
- **Recommendation:** Combined with F2's serialization, process a given child's restack once per convergence rather than once per parent merge; or detect multi-parent children and defer their restack until all parents are `Done`.
- **Effort:** M

## F11: `merge_hub_side` leaves the hub work_dir on `target`
- **Where:** `crates/shelbi-orchestrator/src/actions.rs:867` (`git checkout target`) with no restore
- **Category:** best-practice (side effect on shared state)
- **Severity / Confidence:** low / certain
- **Evidence:** `merge_hub_side` does `git checkout {target}` and never checks back out whatever the hub was on. The PR path (`merge_via_pr`) doesn't move HEAD, so the hub's branch after a `merge` action depends on which path ran — an inconsistent post-condition for a shared checkout.
- **Failure scenario:** An operator or another action assumes the hub sits on its usual branch; after a hub-side merge it's parked on `main` (or an arbitrary `target`), surprising the next manual `git` command run there.
- **Recommendation:** Record the pre-merge branch and restore it in a `finally`-style step, or (better, and this also fixes F2) do the whole merge in a throwaway worktree so the hub's main checkout is never moved.
- **Effort:** S

## F12: `lookup_open_pr` silently picks the first of multiple open PRs
- **Where:** `crates/shelbi-orchestrator/src/git.rs:127` (`lookup_open_pr`)
- **Category:** assumption (branch → at most one open PR)
- **Severity / Confidence:** low / likely
- **Evidence:** The jq expression is `.[0].number // empty` — it takes the first element and drops the rest:

  ```rust
  "--jq", ".[0].number // empty",
  ```

  GitHub does allow more than one open PR for the same head branch (e.g. into different bases, or a cross-fork duplicate). Everything downstream (`ci_watch`, `pr_merge`, `close_pr`) then operates on an arbitrary one with no warning.
- **Failure scenario:** Two open PRs exist for `shelbi/task` (one into `main`, one into `develop`). Zen probes/merges whichever gh lists first, potentially merging into the wrong base while the other PR is silently ignored.
- **Recommendation:** Detect `length > 1` and surface an error (or pick deterministically by base and log the others) instead of blindly taking `.[0]`.
- **Effort:** S

## F13: Danger-path globs are case-sensitive
- **Where:** `crates/shelbi-orchestrator/src/zen.rs:1012` (`match_danger_paths`, `globset::Glob::new`)
- **Category:** hardening (case-variant evasion)
- **Severity / Confidence:** low / likely
- **Evidence:** `Glob::new` builds case-sensitive matchers by default (no `GlobBuilder::case_insensitive(true)`). Git stores exact-case paths, so a path committed as `.github/Workflows/deploy.yml` will not match a `.github/workflows/**` danger glob, even though on a case-insensitive filesystem (macOS default) it resolves to the same file.
- **Failure scenario:** A branch commits a CI change under a case-variant directory; the danger scan misses it and the change auto-merges without the intended warning.
- **Recommendation:** Build the danger `GlobSet` with `case_insensitive(true)`, or normalize both patterns and paths to a consistent case before matching. Document the choice, since it also affects legitimately case-distinct repos.
- **Effort:** S

---

### Notes on things checked and found sound

- **Shell injection via task title / branch / body into PR commit messages** (seed question #5): not reproducible. All argv routes through `run_in_dir` → `shelbi_agent::shell_escape` (single-quote wrapping, `agent/src/lib.rs:72`), and the SSH path single-quotes the whole script (`git.rs:51-58`, regression-tested at `git.rs:322`). The residual risk is *argument* injection, captured in F5, not shell injection.
- **`merge_conflict` probe** correctly uses `git merge-tree --write-tree` (a true merge simulation), so unlike F3 it is not fooled by two-dot divergence and touches no worktree (`zen.rs:862-919`).
- **`delete_branch`** correctly treats `remote ref does not exist` as benign for idempotency and skips branches a workspace still holds (`actions.rs:1052-1063`, `1034-1038`).
- **`restack` already isolates** its rebase in a per-call temp worktree with a unique path (pid + task id + atomic counter, `actions.rs:607-628`) and uses `--force-with-lease` rather than a bare `--force` — the only unqualified force-push in scope, and it is correctly leased.
