# Configurable Review Nav Items

**Status:** draft for jlong review, 2026-08-15. Author: Orchestrator. Not yet
broken into tasks. Aligns with, and is downstream of, [Generic Review via
Workflow Primitives](generic-review-via-workflow-primitives.md) — this plan
moves the *last* hardcoded review UI list into configuration.

## Goal

The review window's action list (today: Chat, View Diff, Edit in Vim, Open
Browser, plus Approve/Reject) is hardcoded in Rust. Make the **launcher items
fully user-defined in the workflow YAML**, so a reviewer can add whatever they
need to run for that project — "Open App" to launch a built desktop binary,
"Run smoke test", "Open Storybook", "Tail logs", anything. Each item is a
**label + an arbitrary shell command**, run in the review worktree with the
review context exported as environment variables.

Decisions locked with jlong (2026-08-15):

- **Surface:** the review window's nav/action items (the "Open in browser /
  Edit in Vi / View Diff / Open App" group). Not the sidebar or palette.
- **Config home:** the **review status block in the workflow YAML** (per
  workflow), alongside the existing serve recipe. Not project.yaml.
- **Item power:** **arbitrary shell commands** (label + `run:`), not a fixed
  menu of built-in action types.

## Motivation

Review actions are project-specific. A web app wants "Open Browser"; a CLI
wants "Run it in a shell"; a desktop app wants "Open the built .app"; a data
job wants "Open the output notebook". Baking a fixed list into the TUI cannot
serve all project types and contradicts the north star of [Generic Review via
Workflow Primitives](generic-review-via-workflow-primitives.md): review should
be *composition of generic primitives*, driven by config, with no
review-specific hardcoding in the crates. The nav list is the last piece of
that list still living in Rust.

## Current implementation (as-is)

Grounded in a source read of the repo at `/Users/jlong/Workspaces/shelbi`
(2026-08-15).

- **The nav list is hardcoded** in `ReviewPanel::rows()` —
  `crates/shelbi-tui/src/review_panel.rs:182`. Items today:
  - `Back` (FocusDashboard), `Status` (inert "Ready for review"), `Folder`
    (reveal worktree in OS file manager).
  - Switch group: `🤓 Chat with Reviewer` (default middle view), `🔀 View Diff`,
    `✍️ Edit in <editor>`, `🌐 Open Browser` — the Browser row is the **only
    config-gated item today**, shown iff the workflow declares a review `url:`
    (`has_review_url`, `review_panel.rs:135,198`).
  - `Actions` section: `✅ Approve`, `❌ Reject`.
- **Enums/rendering:** `SwitchItem` (`:57`), `ActiveView` (`:48`), `PanelRow`
  (`:67`), `PanelEffect` (`:92`, variants FocusDashboard/ShowChat/ShowDiff/
  ShowVim/OpenBrowser/RevealFolder/Approve/RejectPrompt). Labels/glyphs in
  `switch_nav_line()` (`:573-597`). Activation map `activate_row()` (`:298`).
- **Executor:** `run_review_panel()` (`:726`) → `perform_effect()` (`:827`)
  dispatches to `shelbi_orchestrator::review_ui` calls. The middle content
  pane (Chat/Diff/Editor) is swapped via session env vars
  `SHELBI_REVIEW_MID/PANEL/EDITOR/DIFF/CHAT/TASK` (`review_ui.rs:53-66`).
- **Reject reason** is a `tmux display-popup` (`reject_reason_popup()`
  `review_panel.rs:925`) — noted here only because a separate in-flight task is
  making it multi-line; unrelated to nav config.
- **Existing config plumbing to copy:** the review serve recipe already lives
  in the workflow YAML as a `review:` block parsed into `struct ReviewServe`
  (`crates/shelbi-core/src/workflow.rs:1435`, fields `workdir/setup/serve/
  ready/url`), carried to review time via `ResolvedReviewRecipe` (`:1467`) and
  `Workflow::resolved_review_recipe(port)`. `Workflow.review: Option<ReviewServe>`
  at `:145`. Port/slot substitution via `substitute_review_url()` (`:1749`,
  handles `$X` and `${X}`). `ReviewServe` is the natural home for the new field.
- **Tests:** `review_panel.rs` `#[cfg(test)]` (`:991-1457`, ratatui
  `TestBackend`) already asserts nav item presence, Browser gating, editor
  label, ordering, and each activation→effect — the exact surface this plan
  changes.

## Design

### Config shape (workflow YAML `review:` block)

Add an ordered `nav_items:` list to the review block. Each item:

```yaml
review:
  workdir: site
  setup: npm install --no-audit --no-fund
  serve: npm run dev -- -p $SLOT
  ready: curl -sf http://localhost:$SLOT
  url: http://localhost:$SLOT
  nav_items:
    - label: "🔀 View Diff"
      run: git -C "$WORKTREE" diff "$BASE_BRANCH"...HEAD
      target: pane           # render in the review window's middle pane
    - label: "✍️ Edit in Vim"
      run: ${EDITOR:-vim} "$WORKTREE"
      target: pane
    - label: "🌐 Open Browser"
      run: open "$REVIEW_URL"
      target: background     # fire-and-forget; OS takes over
    - label: "🖥 Open App"
      run: open -a "MyApp" "$WORKTREE/build/MyApp.app"
      target: background
```

`ReviewNavItem { label: String (required, may include an emoji/glyph), run:
String (required, arbitrary shell), target: enum {pane, popup, background}
(default: pane), key: Option<String> (optional hotkey) }`.

**`target` — how the command runs** (this is the one place arbitrary-shell
meets the TUI, so it needs a small enum rather than pure free-form):

- `pane` — run in the review window's **middle content pane** (the same slot
  today's in-pane Diff/Editor use). Right for interactive/long-lived commands:
  a pager, an editor, a live diff, `tail -f`. **Default.**
- `popup` — run in a transient `tmux display-popup` overlay. Right for quick,
  self-closing commands.
- `background` — detached fire-and-forget. Right for launchers that hand off to
  the OS (`open …`, desktop-app launch) and return immediately.

**Environment exported to `run`** (real env vars, not string substitution — so
arbitrary shell composes naturally; substitution stays only where it is today,
in `serve`/`url`):

- `WORKTREE` — absolute path to the review worktree.
- `BRANCH` — the task's branch; `BASE_BRANCH` — the workflow base branch.
- `REVIEW_URL` — resolved review url (empty if the workflow declares none).
- `SLOT`, `PORT` — the review slot/port (as the serve recipe already uses).
- `SHELBI_TASK` — the task id; `EDITOR` — the user's editor.

### Defaults & back-compat (must-not-break)

Existing workflows must render exactly as today. Rule:

- **No `review:` block** → diff-only review, unchanged.
- **`review:` block present but `nav_items:` omitted** → synthesize the legacy
  defaults: `View Diff` (pane) + `Edit in <editor>` (pane) + `Open Browser`
  (background, **only if `url:` set**). Chat stays the always-present default
  view (see chrome, below).
- **`nav_items:` present** → it *replaces* the defaults entirely (the user owns
  the launcher group). Document this clearly so an override isn't a surprise.

### What stays framework chrome (not in `nav_items`)

`Back`, `Status`, `Folder` (reveal), the `Actions` section with `Approve` /
`Reject`, and the always-present `Chat with Reviewer` default view remain
first-class and are **not** user-configurable in this plan. Rationale: these
are lifecycle/navigation, not project-specific launchers. Reorder/rename of
Approve/Reject is explicitly out of scope (see Open Decisions).

## Implementation plan (phased)

**Phase 1 — Config model (`shelbi-core`).**
- Add `nav_items: Vec<ReviewNavItem>` to `ReviewServe`
  (`workflow.rs:1435`) and the raw-parse path (`RawWorkflow` review, `:1509`).
  Add `ReviewNavItem` + `ReviewNavTarget` serde structs (copy the optional
  sub-block pattern from `WorkflowZenConfig` `:1364`).
- Carry into `ResolvedReviewRecipe` (`:1467`). Do **not** pre-substitute `run`
  — resolution is env export at run time.
- Validation (workflow validate + `config_inventory_lint`): non-empty
  `label`/`run`; unique `key`s; known `target`. Empty list == omitted.
- Default synthesis per the back-compat rule above.

**Phase 2 — Plumbing (`shelbi-orchestrator`).**
- `review_context()` (`review_panel.rs:757`) / `review_ui.rs`: resolve the
  loaded task's workflow review nav items and pass the list into
  `ReviewPanel::new()` (currently takes only `has_review_url`).
- New `review_ui` executor `run_review_nav_item(item, ctx)`: `cd` into
  worktree, export the env set above, run `run` per `target` (middle pane via
  the existing `SHELBI_REVIEW_MID` mechanism / `tmux respawn-pane`; `popup` via
  `display-popup`; `background` via detached spawn).

**Phase 3 — TUI (`shelbi-tui/review_panel.rs`).**
- Replace the hardcoded switch group in `rows()` (`:182`) with the config
  list; keep Back/Status/Folder + Actions + Chat as chrome.
- Generalize `PanelEffect` (`:92`): add `RunNavItem(index)`; keep
  FocusDashboard/RevealFolder/Approve/RejectPrompt. Render label/glyph straight
  from config in `switch_nav_line()` (`:573`). Optional per-item hotkey.

**Phase 4 — Tests + docs.**
- Update `review_panel.rs` tests (`:991`): defaults reproduce today's list;
  custom `nav_items` render + activate → `RunNavItem`; omitted vs. present.
- `workflow.rs` parse/validation tests for `nav_items`.
- Docs: `site/content/docs/configuration/workflow.mdx` review section (`:206`)
  — document `nav_items` shape, the exported env vars, the three `target`
  modes, and the "present replaces defaults" rule.

## Open decisions for jlong

1. **In-pane built-ins vs. pure config.** Recommended (above): `nav_items`
   fully owns the launcher group; Diff/Edit/Browser ship as *default config
   entries* reproducing today's behavior; Chat stays an always-present view.
   Alternative: keep Diff/Edit/Chat as first-class in-pane views and let
   `nav_items` only *append* extra launchers. Recommendation: former (cleaner,
   matches the generic-primitives north star).
2. **Env export vs. `$SLOT`-style substitution for `run`.** Recommended: real
   exported env vars (arbitrary shell composes cleanly), keeping `$SLOT/$PORT`
   substitution only in `serve`/`url` as today.
3. **Approve/Reject reorder/rename.** Out of scope here (stay chrome). Revisit
   if a project needs it.
4. **Per-item hotkeys.** Include `key:` now, or defer? Low cost to include.
5. **Security note (not a blocker).** `run` executes with the reviewer's
   privileges in the worktree. This is author-controlled config (like a git
   hook the user writes), not remote input — acceptable, but worth one line in
   the docs. Note the workflow YAML is already a Zen danger-path, so changes to
   it get a human gate.

## Definition of done

- A workflow can declare `review.nav_items`; the review window renders exactly
  those launcher items with their labels, each running its `run` command in the
  worktree with the documented env, dispatched per `target`.
- Omitting `nav_items` reproduces today's Diff/Edit/Browser list; no existing
  workflow changes behavior.
- `grep` for hardcoded launcher labels (`"View Diff"`, `"Open Browser"`, etc.)
  in `crates/` finds them only as *default config synthesis*, not as a fixed
  UI list — consistent with the generic-primitives north star.
- Tests cover default synthesis, custom items, and each `target` mode; docs
  updated.
