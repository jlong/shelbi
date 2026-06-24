# Zen Mode

## Context

Today Shelbi's orchestrator is the *scheduler* but the user is still the *priority-setter* and *reviewer*. The orchestrator only dispatches tasks that the user has promoted from backlog to todo, and it only marks tasks done after the user has accepted them (or after the orchestrator has merged the work into main, per the autonomous-merge feedback memory). The user remains in the loop for both directions of the kanban: which work to start, and whether the result is good.

The next step is **Zen Mode** — flip the orchestrator from "scheduler" to "lead". When Zen Mode is on, the orchestrator:

- **Triages the backlog itself.** Picks which tasks to promote to todo based on priority, age, dependencies, and what's currently in flight. The user can still pin / un-pin / demote anything, but they don't *have to*.
- **Merges high-confidence work automatically.** When a finished branch passes the safety bar — local tests green, GitHub checks green — the orchestrator squash-merges into main and moves the task to done without waiting for the user.
- **Surfaces only the ambiguous cases.** Test failures, conflicts that need taste, design-level decisions, anything outside the safety bar — those still land in review for the user.

The result is a workflow where the user can drop a stack of tasks into backlog, type "zen on," and walk away. They get a digest of what landed, what's stuck, and what needs them.

## Design

### 1. Activation

Zen Mode is a **per-project runtime flag**, not a configuration file. Reasons:

- The user wants to flip it on for a specific stretch of work and flip it off when they need a tighter loop.
- A YAML flag invites stale config (left on by accident, surprise auto-merges weeks later).
- A runtime toggle is naturally visible in the TUI status footer.

Surface as:

- `shelbi zen on` / `shelbi zen off` / `shelbi zen status` — CLI control.
- **Hotkey toggle in the TUI** — **Alt+Z** (⌥Z on Mac). Single chord, flips Zen in place, like Shift+Tab flips permission modes in Claude. Pressing the chord re-paints the indicator + emits an event-log line.
- **Sidebar indicator (when Zen is on).** A pill in the lower-left status block of the sidebar, *replacing* the current selection-state line (`> orch`) and slotted under the `^P palette  q quit` keymap row. Pill style: **green background, black text** ("ZEN ON"). The pill is absent when Zen is off — no chrome wasted on the off state.
- Persisted in `~/.shelbi/projects/<project>/state.json` so a TUI restart preserves the setting (but a fresh `shelbi reload` doesn't auto-enable; explicit user action only).

Sketch of the sidebar lower-left when Zen is on:

```
  ^P palette  q quit
  ┌─────────┐
  │ ZEN ON  │   ← green bg, black text
  └─────────┘
```

When off, that block is empty — keeps the chrome quiet 99% of the time.

The orchestrator reads the flag on every dispatch / merge decision; flipping it off mid-flow stops further auto-actions immediately (in-flight workers keep going; their results just land in review instead of done).

**Crash recovery.** If the orchestrator pane dies, on restart it checks `state.json` for a recent crash timestamp. If a crash is detected within the last hour, Zen Mode auto-disables and the orchestrator surfaces a clear warning: *"Zen disabled after crash; re-enable manually if you want to resume."* In-flight workers (which don't know about Zen) keep going and their results land in `review` for the user to inspect. This is the safe default — no surprise auto-merges after a crash, and the user is forced to do a quick check-in before resuming autonomous mode.

### 2. Auto-promotion from backlog

When a worker frees up and todo is empty, the orchestrator considers promoting from backlog. Promotion rules:

- **Dependency-clean.** Skip tasks whose `depends_on` still has open items.
- **No file-overlap conflict** with anything currently in flight. Compute the task's likely write surface from the task body — if it mentions files already being touched by an `in_progress` task, hold.
- **Not opt-out.** A task with `zen: false` in its frontmatter is never auto-promoted. Useful for "let me discuss this one first" markers.
- **Highest-priority eligible task first.** Existing priority field drives the order.
- **Sanity bound on parallel auto-promotes.** Don't promote more than the number of free workers in one pass; never auto-promote a task that has already been promoted-then-demoted by the user (that's a clear "stop touching this" signal).

Auto-promotion emits an event-log line tagged `reason=orchestrator:zen-promote`. The activity feed surfaces these visibly — they're the kind of action a user wants to spot-check.

### 3. The high-confidence bar

A finished branch (one that landed in review via review-marker) gets auto-merged when **all** of these are true:

- **Local test suite passes** on the worker's worktree. The check commands live in two places:

  **Project-wide baseline** in `~/.shelbi/projects/<project>.yaml`:

      zen:
        checks:
          local:
            - "cargo test --workspace"
            - "cargo clippy --workspace --all-targets -- -D warnings"

  **Per-task override** in task frontmatter (optional). Two shapes:

      # Extend the project baseline with extra checks:
      zen:
        checks_additional:
          - "cargo test --package shelbi-tui"

      # Or REPLACE the baseline entirely (e.g., docs-only task):
      zen:
        checks_only:
          - "npm run lint"

  All resolved commands run sequentially on the worker's worktree after the review marker drops, before push. If any exits non-zero, abort the auto-merge. The escape hatch is real — workers can effectively turn off testing per task — so `checks_only` is a deliberate choice that's logged to the events feed.

- **No merge conflicts** with current main. Standard squash-merge check; if it conflicts, fall back to user review.

- **No size red flag.** If the diff touches more files / more lines than a threshold (e.g., > 2000 lines or > 30 files), surface to user instead. Big diffs warrant a human glance even when tests pass.

- **No "danger" file paths touched.** Hardcoded list of paths that always require human review:
  - `.github/workflows/**` (CI changes)
  - `scripts/install.sh` (anything installers rely on)
  - `**/*.yaml` and `**/*.yml` in project roots (config drift)
  - `LICENSE`, `package-lock.json` mass-change, `Cargo.lock` mass-change
  - The list is configurable per project; sensible default ships.

- **GitHub checks pass** within a timeout (default **15 minutes**, configurable per project via `zen.ci_timeout`). After local checks succeed:
  1. Push the branch to origin (do not squash-merge yet).
  2. Open a PR via `gh pr create`.
  3. Wait for `gh pr checks --watch` to report all-success.
  4. Squash-merge via `gh pr merge --squash`.
  5. Delete branch, mark task done.

If GitHub checks fail or time out, the orchestrator surfaces the failure in the activity feed and leaves the task in `review` with a `reason=zen:failed-checks` event tag — no special column, just the review pane the user already uses. The events feed is the audit surface.

### 4. The merge flow: PR-based, always

Every Zen merge goes through a GitHub PR. CI runs on the PR. The PR is auto-merged on green via `gh pr merge --squash`. This gives a clean audit trail in GitHub itself and lets external CI services (Vercel previews, etc.) gate the merge naturally.

No "direct to main with revert-on-fail" alternative — that path was considered and rejected because (a) it would auto-deploy bad merges to Vercel between merge and revert, and (b) revert commits clutter the history. PR-based is the only mode.

### 5. Worker idle behavior

Today, when todo is empty and workers are free, they sit idle. Under Zen Mode they keep busy:

- The orchestrator checks backlog every time a worker frees up (and on a periodic tick, e.g., every 60s) to see if anything new is auto-promotable.
- If backlog is empty too, workers genuinely idle. The orchestrator does NOT invent work.
- If there's auto-promotable work blocked only by file-overlap conflicts with in-flight work, the orchestrator waits for the conflict source to land.

### 6. The activity feed in Zen Mode

Zen-driven events are **visually distinct** in the feed — a small `zen` badge in the avatar column and a subtle tinted-row background (gray-1) so the user can scan in two seconds for "what did the orchestrator do without me." Same one-line structure as user actions; just marked.

Example feed (rendered with the agent-avatars and zen-tint treatment):

```
███ zen   promoted  "Sidebar bug"            2m ago
          backlog → todo

▲   alpha started "Sidebar bug"              3m ago

███ zen   merged    "Footer expand"          7m ago
          tests green · ci green

⚠   zen   bailed on "Big refactor"           12m ago
          diff too large · 41 files / 3,200 lines

✓         "Marketing copy" accepted           1h ago
```

Zen-specific event kinds added to the events log:

- `task=<id> backlog -> todo reason=orchestrator:zen-promote`
- `task=<id> review -> done reason=orchestrator:zen-merge`
- `task=<id> review reason=zen:failed-checks <summary>` — stays in review with ⚠.
- `task=<id> review reason=zen:diff-too-large <stats>` — same.
- `task=<id> review reason=zen:danger-path <paths>` — same.
- `task=<id> review reason=zen:ci-timeout` — same.
- `task=<id> review reason=zen:merge-conflict` — same.

A "Zen activity" filter / pill on the feed shows just these events so the user can audit the orchestrator's autonomy at a glance.

### 7. Stop-the-world levers

If something goes wrong, the user has three escape hatches:

- **`shelbi zen off`** — immediate. New decisions stop; in-flight workers finish and their results go to review.
- **`shelbi zen pause`** — softer. New auto-promotions stop, but in-flight Zen merges complete. Useful when the user wants to inspect the current state without unwinding it.
- **Per-task** `shelbi task move <id> --to backlog` from review — pulls a Zen-flagged task out of the auto-merge path even if it would have passed checks. Useful for late-stage objections.

### 8. Pre-flight: dry-run mode

Before flipping Zen on for real work, the user can run `shelbi zen --dry-run` which simulates the orchestrator's decisions for the next hour (or until ctrl-c). It logs "would have promoted X / would have merged Y" lines without actually doing anything. Catches misconfigured check commands and surprise auto-promotions before they happen.

## Rollout

Two phases, each independently shippable.

**Phase 1 — Full Zen (auto-promote + auto-merge).**

The whole loop ships in one phase per user direction — "drop tasks, walk away" from day one.

- Add `zen.checks.local`, `zen.ci_timeout`, `zen.danger_paths` to project YAML schema. Default `checks.local` empty (no checks → no auto-merge); default `ci_timeout` 15 minutes.
- Add `zen.checks_additional` / `zen.checks_only` to task frontmatter schema.
- Implement the high-confidence bar (local tests + merge-conflict check + diff size + danger paths + GitHub PR checks).
- Implement the PR-based merge flow via `gh pr create` / `gh pr checks --watch` / `gh pr merge --squash`.
- Implement auto-promotion: dependency-clean, no file-overlap with in-flight, `zen: false` opt-out, priority order, sanity-bounded.
- `shelbi zen on/off/pause/status` CLI commands.
- Hotkey toggle in the TUI (chord TBD — see Decisions).
- TUI footer indicator: `Mode: Zen` in green when on.
- Activity feed: visually-distinct `zen` rows (badge + tinted background), new event kinds tagged `reason=orchestrator:zen-*` and `reason=zen:*`.
- Crash recovery: detect recent-crash timestamp on restart, auto-disable Zen, warn user.
- State persistence in `~/.shelbi/projects/<project>/state.json`.

After Phase 1: drop a stack of tasks into backlog, hit the toggle hotkey, walk away.

**Phase 2 — Polish + dry-run.**

- `shelbi zen --dry-run` simulation mode (logs "would have promoted X / merged Y" for an hour without doing anything).
- Zen activity filter / pill on the feed view.
- Per-project danger-path overrides + smarter defaults beyond the hardcoded list.
- Stop-the-world levers refined based on real usage feedback from Phase 1.
- Anything that didn't land in Phase 1.

## Decisions

(All seven open questions closed during plan review.)

- **Activation:** runtime flag (`shelbi zen on/off`), TUI footer indicator, and **Alt+Z** (⌥Z on Mac) hotkey chord that toggles Zen in place. Persisted in `state.json`. No YAML enablement.
- **Hotkey under modals:** swallowed. While the palette / popup has focus, Alt+Z does nothing — the user closes the modal first, then toggles. Matches the behavior of Shift+Tab in Claude.
- **Fallback chord for incompatible terminals:** first-run probe detects whether Alt is passed cleanly. If not, the user is prompted to pick a fallback (`Ctrl+\\`, `Ctrl+G`, `Ctrl+Shift+Z`, or skip and use the CLI). Persisted to `~/.shelbi/config.yaml` under `keymap.zen_toggle`.
- **`zen.danger_paths` extend by default.** Project-listed paths are appended to the hardcoded built-in list (`.github/workflows/**`, `scripts/install.sh`, root `*.yaml`/`*.yml`, `LICENSE`, lockfiles). To replace entirely, set `override: true` and supply a `paths:` array. Replacing is the deliberate-opt-out shape; the safe default is additive.
- **Phasing:** full Zen v1 — both auto-promote from backlog AND auto-merge of high-confidence work ship in Phase 1 together. Faster trust ramp.
- **Merge path:** PR-based, always. `gh pr create` → `gh pr checks --watch` → `gh pr merge --squash`. No direct-to-main alternative.
- **Failure surface:** Zen-bailed tasks stay in `review` with a ⚠ icon and `reason=zen:*` tag in the events feed. No new column.
- **Check config:** project YAML baseline (`zen.checks.local`) plus per-task frontmatter override (`zen.checks_additional` or `zen.checks_only`).
- **CI timeout:** 15 minutes default, configurable per project via `zen.ci_timeout`.
- **Activity feed voice:** Zen events are visually distinct — `zen` badge in the avatar column + subtle tinted-row background. Same one-line shape as user actions; easy to scan as a separate stream.
- **Crash recovery:** on orchestrator restart after a recent crash (within 1h), Zen auto-disables and surfaces a warning. In-flight workers' results land in review as normal. User must re-enable manually.

## Open questions

_None remaining — plan is ready to execute. Phase 1 sub-tasks (the exact wording of the first-run fallback prompt, the precise event-feed icon set, etc.) will surface during implementation but don't need pre-commitment._

## Open questions

- **Per-project vs. per-task check commands.** Should `checks.local` be project-wide only, or can a task specify additional checks in its frontmatter? Erring toward project-wide for v1 to keep the schema small.
- **PR vs. no-PR default.** Recommending PR-based; confirming.
- **Auto-promote at all in v1?** Phase 1 ships auto-merge only and leaves backlog triage to the user. That's the bigger trust ask, and starting narrower means the user gets to see the orchestrator's judgment on a small surface before handing it more rope. Confirm phasing.
- **What's the failure surface?** When a Zen merge bails (checks fail, conflict, danger path, size), it stays in review. Should we add a new column — "needs-human" — to visually separate "agent has nothing more it can do" from "regular review"? Or is a sidebar badge enough?
- **CI timeout.** How long does the orchestrator wait for GitHub checks before giving up? Suggest 15min default, configurable.
- **Revert-on-fail in no-PR mode.** Want to ship this in Phase 1 too, or push to Phase 3 polish?
- **Tone of the activity feed entries.** Should Zen-promote / Zen-merge events read in the same voice as user actions, or be visually distinct so the user can scan for "what did the orchestrator do without me"?
- **What about the orchestrator itself failing under load?** If Zen is on and the orchestrator pane crashes, what happens to in-flight Zen tasks? Probably they stay in their current column and the user sees them on restart, but worth thinking through.
