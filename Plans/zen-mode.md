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

**How the orchestrator learns about toggles.** Two channels:

1. **Bootstrap.** On session start, the orchestrator reads `state.json::zen_mode` once. This sets the initial value of its in-memory `zen_active` boolean. No event replay needed — `state.json` is the source of truth for "what's the current state right now."

2. **Live (mid-session).** Every toggle path — `shelbi zen on/off/pause`, the Alt+Z hotkey, the crash-recovery auto-disable — emits a new event-log line shape:

       <ts> mode=zen <prev> -> <new> reason=<source>

   Where `<source>` is one of `user:cli`, `user:hotkey`, `system:crash-recovery`. The orchestrator is already tailing `events.log --follow` for task and worker events; `mode=` lines flow through that same stream. Its reaction rules gain a bullet for the new shape:

   > On `mode=zen * -> on`: print the current board + worker snapshot, switch to Zen reaction rules (auto-merge on review, evaluate auto-promote candidates per the Zen Mode prompt section).
   >
   > On `mode=zen * -> off` (or `* -> paused`): stop initiating new Zen actions. In-flight Zen merges complete; their results land per the normal flow.

This matches how the orchestrator already tracks worker state via the tail — bootstrap once, react to deltas thereafter. No polling.

**Crash recovery.** If the orchestrator pane dies, on restart it checks `state.json` for a recent crash timestamp. If a crash is detected within the last hour, Zen Mode auto-disables and the orchestrator surfaces a clear warning: *"Zen disabled after crash; re-enable manually if you want to resume."* In-flight workers (which don't know about Zen) keep going and their results land in `review` for the user to inspect. This is the safe default — no surprise auto-merges after a crash, and the user is forced to do a quick check-in before resuming autonomous mode.

### 2. Auto-promotion from backlog

Auto-promotion is a **two-stage decision**: the Rust side computes a list of *mechanically eligible* candidates; the orchestrator applies *judgment* to pick which (if any) of those to actually promote. The judgment lives in the orchestrator prompt so the user can tune it per project.

**Stage 1 — mechanical eligibility (Rust, in `shelbi zen scan`).** Returns the list of tasks that are safe to promote from a state-machine standpoint:

- **Dependency-clean.** All `depends_on` entries are `done`.
- **Not opt-out.** Frontmatter doesn't have `zen.enabled: false`.
- **Not previously demoted.** The events log has no `task=<id> todo -> backlog reason=user:*` line. Once the user has explicitly pulled a task back, Zen never re-promotes it.
- **No file-overlap with in-flight work.** Heuristic: path-like tokens in the task body don't appear in any `in_progress` task's body.
- **Sorted by priority** (lower = higher).

The Rust call returns the candidate list. Nothing is promoted yet — this is *what could be*, not *what should be*.

**Stage 2 — judgment (orchestrator prompt).** Given the candidate list, the orchestrator decides which to actually promote based on the user's intent. The orchestrator prompt template ships with a default "Zen Mode" section that lists three categories of work the orchestrator may auto-promote:

> Only auto-promote a candidate task if **at least one** of these is true:
>
> 1. **It's the kind of work the user generally trusts you with.** Look at the done column — does the user routinely accept tasks of this shape without changes? (Examples: docs typo fixes, dependency bumps, content sweeps that match a recently-stated convention.) If their done-history shows pattern acceptance, this is in scope.
> 2. **It's part of fixing an issue the user recently raised.** Did the user mention this bug, feature, or concern in conversation in the last few turns? Tasks that respond to something the user explicitly asked for are in scope.
> 3. **It's part of a larger body of work the user explicitly kicked off.** If the user filed a batch of related tasks (e.g. 13 vs-pages, a multi-step refactor), the remaining items in that batch are in scope.
>
> If a candidate fits none of these, **leave it in backlog** and surface it in your next user-facing reply: *"I considered promoting `<task>` but wasn't sure if it fits your intent — want me to?"* That keeps you out of trouble while still telling the user what's available.

This section is editable. The user can:
- Add categories ("anything tagged `automation:` in the task title is always in scope").
- Tighten categories ("only auto-promote if the user has accepted ≥ 3 tasks of this shape without changes").
- Replace the list entirely with their own taxonomy.

The Rust side never inspects these criteria — it just supplies the candidate list. The orchestrator prompt is where the user owns the policy.

**Event emission.** When the orchestrator promotes a task, it emits `reason=orchestrator:zen-promote category=<which-of-the-three>`. When it declines a candidate, it emits `reason=orchestrator:zen-decline reason-text=<short-explanation>`. The activity feed renders both — the user sees what was promoted AND what was considered.

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

- `task=<id> backlog -> todo reason=orchestrator:zen-promote category=<which>` — promoted by judgment.
- `task=<id> backlog reason=orchestrator:zen-decline reason-text=<short>` — was mechanically eligible, judgment said no.
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
- Alt+Z hotkey toggle in the TUI (with first-run probe + fallback-chord picker for incompatible terminals).
- Sidebar indicator pill (green bg, black text, "ZEN ON") in the lower-left status block, replacing the current selection-state line.
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

- **Activation:** runtime flag (`shelbi zen on/off`), **Alt+Z** (⌥Z on Mac) hotkey chord that toggles Zen in place, and a sidebar indicator pill (green background, black text, "ZEN ON") in the lower-left status block that replaces the current selection-state line. The pill is absent when Zen is off. Persisted in `state.json`. No YAML enablement.
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
- **Auto-promotion is judgment, not a mechanical rule.** The Rust `shelbi zen scan` returns mechanically-eligible candidates only (deps clean, not opted-out, not user-demoted, no file-overlap). The orchestrator prompt's "Zen Mode" section applies the user-tunable judgment: only auto-promote if the task fits one of three categories — (1) work the user generally trusts the orchestrator with, (2) part of fixing a recently-raised issue, or (3) part of a larger body of work the user explicitly kicked off. Candidates that don't fit are surfaced as "I considered but didn't promote — want me to?" Users edit the prompt section to tune categories per project.

## Open questions

_None remaining — plan is ready to execute. Phase 1 sub-tasks (the exact wording of the first-run fallback prompt, the precise event-feed icon set, etc.) will surface during implementation but don't need pre-commitment._
