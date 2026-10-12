# rt-merge-main-into-umbrella

Status: **done** — merged `origin/main` (5 new commits as of 2026-10-12) into
the `jlong/remove-tmux` umbrella with a real merge commit; conflicts resolved so
both sides' intent survives.

## Main commits brought in

- `3ab7f816` fix(release): wait for APT repo to settle before verify install (#1524)
- `a3338478` fix(poller): deflake low_disk_free_bytes threshold test (#1541)
- `cf5911aa` fix(poller): lease handoff force-push against the fresh remote tip and cap retries (#1583)
- `d2abe8d6` refactor(worktree): move per-worktree markers out of .claude/ into .shelbi/ (#1609)
- `cd894d35` docs(site): point per-worktree marker paths at .shelbi/ not .claude/ (#1611)

## Conflicts and how each was resolved

1. **`crates/shelbi-cli/src/commands/spawn.rs`** — modify/delete. The umbrella
   deleted the whole file in the tmux cutover (#1505, 610 lines); its only
   function touched by main was `ensure_gitignored`, whose #1609 change was a
   gitignore-probe swap (`.claude/shelbi-ready` → `.claude/settings.json`) plus
   dropping the obsolete `.claude/shelbi-ready` gitignore line. `ensure_gitignored`
   exists **nowhere** on the umbrella side (confirmed by grep across the HEAD
   tree), so the change patched code the umbrella already removed. **Dropped**;
   file stays deleted.

2. **`crates/shelbi-orchestrator/src/poller.rs`** — content conflict (poller
   moved tui→orchestrator crate on the umbrella, so main's
   `shelbi_orchestrator::workspace::` paths had to become `crate::workspace::`).
   Kept the umbrella's session-backend logic and layered main's additions on top:
   - #1609 ready- and transition-marker legacy fallbacks: `let mut marker` +
     `legacy_workspace_{ready,transition}_marker`, read the legacy `.claude/`
     path when the new `.shelbi/` marker is absent, and clear whichever file held
     the signal.
   - #1583 handoff force-push lease: `workspace_push_blocked_marker`, the
     tip-keyed `PushRetryLedger`, `PushRetryLedger::blocks` early-skip, and the
     `decide_push_failure` → Retry/Block ledger write-back, all present.
   - Rewrote the merged-in main lines' `shelbi_orchestrator::workspace::`
     references to `crate::workspace::` (poller now lives in that crate).
   - Doc comment updated from `.claude/shelbi-ready` to `.shelbi/shelbi-ready`.
   - Adapted main's `legacy_claude_ready_marker_still_promotes_and_clears` test
     to the umbrella world: `crate::test_support::ENV_LOCK` → `crate::test_lock::acquire()`,
     and `TmuxAddr { session, window }` → `SessionTarget::slot("s", "w")`.

3. **`crates/shelbi-tui/src/app.rs`** — content conflict in the
   `review_workspace_is_serving` doc comment. Kept the umbrella's richer doc
   (which explains the delegation to `review_slot_is_serving` and the URL-less
   review case) and took main's marker-path update: `.claude/shelbi-review-loaded`
   → `.shelbi/shelbi-review-loaded`.

## #1609 marker move applied to umbrella-only code

After the merge I grepped the whole tree for `.claude/shelbi-*` marker paths.
All six path builders in `workspace.rs` already resolve to `.shelbi/` (auto-merged
from #1609), and the two `legacy_workspace_{ready,transition}_marker` builders
intentionally resolve to `.claude/` for the in-flight-worker read fallback. The
dispatch prompt embeds the absolute path from the builder, so the worker-facing
handoff text self-updated. The review template already names
`.shelbi/shelbi-review-loaded`. The remaining fixes to umbrella-only code (that
#1609 never saw because it didn't exist on main):

- `workspace.rs` `review_slot_serving` / `review_slot_is_serving` doc comments:
  `.claude/shelbi-review-loaded` → `.shelbi/shelbi-review-loaded` (the builder
  these delegate to already writes `.shelbi/`).
- `app.rs` doc comment (above).

No production code writes a per-worktree marker under `.claude/` except the two
legacy read-fallback builders. Reads accept the legacy `.claude/` ready and
transition paths, exactly as on main. (The lone remaining `.claude/shelbi-review-loaded`
mention in `docs/removing-tmux/status/rt-a-review-with-a-live-review-agent-...md`
is a historical status record from an earlier subtask, not code; left as-is.)

## Dropped main changes

- #1609's `spawn.rs::ensure_gitignored` gitignore-probe change — the function
  was deleted with `spawn.rs` in the tmux cutover; nothing to port.

## Verification

- `cargo build --workspace` — green.
- `cargo clippy --workspace --all-targets -- -D warnings` — green.
- `cargo test --workspace` — green (see commit message for any noted flakes).
- `git merge-tree --write-tree origin/main HEAD` — clean (no conflicts) after
  the merge; every listed `origin/main` commit is an ancestor of the branch.
- Key behaviors intact on the session backend: #1623 review-slot hysteresis /
  crash-cap, #1583 push lease + retry cap, #1609 legacy marker fallback, #1541
  deflake, #1470 idle-only heartbeat count.
