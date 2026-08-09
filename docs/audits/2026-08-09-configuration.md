# Configuration docs audit (2026-08-09)

Audit of the four Configuration reference pages under
`site/content/docs/configuration/` against the actual config schema,
`shelbi init` scaffold output, and serde serialization. **Audit only — no
doc file was modified.** A separate fix pass follows triage.

Pages covered:

- `site/content/docs/configuration/global.mdx`
- `site/content/docs/configuration/project.mdx`
- `site/content/docs/configuration/statuses.mdx`
- `site/content/docs/configuration/workflow.mdx`

Ground truth read: `crates/shelbi-core/src/scaffold.rs`,
`crates/shelbi-core/src/model.rs`, `crates/shelbi-core/src/workflow.rs`,
`crates/shelbi-core/src/statuses.rs`, `crates/shelbi-cli/src/commands/init.rs`,
`crates/shelbi-state/src/lib.rs`,
`crates/shelbi-state/src/project_paths.rs`,
`crates/shelbi-state/src/user_config.rs`,
`crates/shelbi-state/src/keymap/{loader,actions}.rs`.

Prior audit for dedupe: `docs/documentation-codebase-audit.md` (2026-07-13),
finding H1 (the `local.yaml` split layout). Re-verified against current
`scaffold.rs` / `init.rs` below — **still present** (F1).

---

## F1 — `local.yaml` split layout is not what fresh or picked-up in-repo projects use

- **Severity:** high
- **Type:** confirmed error (re-confirms prior audit H1 against current code)
- **Doc file + lines:** `project.mdx:17-20` (the "Where it lives" table) and
  `project.mdx:36-43` (prose).
- **Doc claim:** In-repo mode splits config into two files — *shared* fields
  (`name`, `default_branch`, `default_workflow`, `orchestrator`,
  `agent_runners`, `zen`, …) committed at `<repo>/.shelbi/project.yaml`, and
  *user-local* fields (`repo`, `machines`, `workspaces`, `editor`) in a
  per-machine `~/.shelbi/projects/<id>/local.yaml`. Presented as the layout
  every in-repo project uses.
- **Contradicting source:**
  - `crates/shelbi-cli/src/commands/init.rs:437-503`
    (`render_project_yaml`) renders **one flat file** carrying *all* fields
    (shared + user-local: `repo`, `machines`, `workspaces`, `orchestrator`,
    `agent_runners`, …), and `init.rs:592-611` writes it to the flat
    `~/.shelbi/projects/<id>.yaml` even when `config_mode: in-repo`.
  - `crates/shelbi-cli/src/commands/init.rs:678-705` (`write_in_repo_config`)
    writes the committed `<repo>/.shelbi/project.yaml` with **only** `name`
    (+ optional `display_name`) — *not* the shared-field bucket the docs
    describe (see its own comment at `init.rs:670-677`: "The shape stays
    minimal on purpose").
  - `crates/shelbi-cli/src/commands/init.rs:832-847` (`run_pick_up`) also
    writes the **flat** `~/.shelbi/projects/<alias>.yaml` via
    `render_project_yaml`; it never writes `local.yaml`.
  - `crates/shelbi-state/src/lib.rs:775-799` (`load_project`) reads the flat
    file first and only falls back to `load_project_split`
    (`lib.rs:969-998`, which reads `local.yaml` + committed `project.yaml`)
    when the flat file is **absent**.
  - No init path writes `~/.shelbi/projects/<id>/local.yaml`. The split is
    produced only by the migration path
    (`crates/shelbi-state/src/migrate.rs:188-264`) or by a later
    `shelbi workspace` edit on an in-repo project
    (`crates/shelbi-cli/src/commands/workspace.rs:668-684`).
- **Impact:** A user who runs `shelbi init --mode in-repo` (or `--pick-up`)
  is told to edit `~/.shelbi/projects/<id>/local.yaml` for `repo` / `machines`
  / `workspaces` / `editor` and to expect shared fields in the committed
  `project.yaml`. Neither is true for that project: everything lives in the
  flat `~/.shelbi/projects/<id>.yaml`, and a hand-created `local.yaml` is
  shadowed by the flat file the loader prefers. The shared/user-local
  validation boundary the docs promise is not applied to fresh/picked-up
  projects.
- **Note:** The *bucket contents* the docs list do match
  `SHARED_PROJECT_FIELDS` / `LOCAL_PROJECT_FIELDS`
  (`crates/shelbi-core/src/model.rs:177-197`). The error is that the
  two-file split those buckets drive is not the layout init/pick-up
  produce. This is the same defect flagged in the 2026-07-13 audit (H1) and
  is unresolved in current code.

## F2 — Heartbeat defaults are stale (docs say `3m` / `60m`; code uses `60s` / `5m`)

- **Severity:** high
- **Type:** confirmed error (drift from PR #521 "retire the 30s keepalive")
- **Doc file + lines:** `project.mdx:92` (top-level field table, "Default"
  column = `3m` / `60m`); `project.mdx:278-279` (idle back-off example
  `3m → 6m → 12m → … → 60m`, "resets to `3m`"); `project.mdx:285` (bare
  `heartbeat: 3m` "keeps the default `max` (`60m`)"); `project.mdx:293`
  (`interval` default `3m`); `project.mdx:294` (`max` default `60m`).
- **Doc claim:** Heartbeat `interval` defaults to `3m` and `max` to `60m`; an
  idle hub relaxes `3m → 6m → 12m → … → 60m`.
- **Contradicting source:**
  - `crates/shelbi-core/src/model.rs:310` —
    `HEARTBEAT_DEFAULT: Duration = Duration::from_secs(60)` (60s, not 3m).
  - `crates/shelbi-core/src/model.rs:316` —
    `HEARTBEAT_MAX_DEFAULT: Duration = Duration::from_secs(300)` (5m, not
    60m).
  - `crates/shelbi-core/src/model.rs:318-325` — `Default for HeartbeatConfig`
    uses those two constants; `model.rs:313-314` gives the real back-off
    ladder `60s → 2m → 4m → 5m`.
  - `crates/shelbi-core/src/scaffold.rs:169-176` — the scaffolded project
    file's own comment states "Defaults: interval 60s, max 5m." and its
    example uses `interval: 60s` / `max: 5m`.
- **Impact:** Every documented default and the worked back-off example are
  wrong. Examples still *parse* (the values are legal), so this is
  misleading rather than breaking, but a user reasoning about idle cadence
  from the docs gets numbers 3x/12x too large.

## F3 — global.mdx documents a `review` keymap mode (and its actions) that does not exist

- **Severity:** high
- **Type:** confirmed error
- **Doc file + lines:** `global.mdx:104` (the "Modes" list names seven modes
  incl. `review`); `global.mdx:129-131` (an Actions-table block for mode
  `review`: `nav_up`/`nav_down`, `scroll_body_up`/`scroll_body_down`,
  `activate`).
- **Doc claim:** `review` is a valid top-level mode key under `defaults` /
  `projects.<name>` in `keys.yaml`, carrying `nav_up`, `nav_down`,
  `scroll_body_up`, `scroll_body_down`, `activate`.
- **Contradicting source:**
  - `crates/shelbi-state/src/keymap/loader.rs:48-55` — `Keymaps` has exactly
    six modes: `global`, `sidebar`, `kanban`, `popover`, `activity`,
    `palette`. No `review`.
  - `crates/shelbi-state/src/keymap/actions.rs:239-244` — the mode-name
    mapping enumerates the same six; there is no `ReviewAction` enum and no
    `review` arm.
  - `keys.yaml` modes are parsed as untyped string keys
    (`loader.rs:216` `ModeMap = HashMap<String, HashMap<String, Value>>`), so
    a `review:` block parses but routes to no mode — its bindings never take
    effect.
- **Impact:** A user who adds `defaults.review.nav_up: …` (as the doc's own
  modes list invites) gets a silently inert binding. The five `review`-mode
  actions listed are not real config keys. (The six other modes and the
  spot-checked action names — `open_palette`, `move_card_left`,
  `reorder_up`, `cycle_workflow_filter`, `open_popover`, `reset_filter`,
  `toggle_zen_filter`, `toggle_workspaces_filter` — do exist:
  `actions.rs:256-298`.)

## F4 — workflow.mdx "default.yaml" example diverges from the shipped default workflow

- **Severity:** medium
- **Type:** drift (example labeled as the scaffolded file but does not match it)
- **Doc file + lines:** `workflow.mdx:28-44` (the Example block, headed
  `# ~/.shelbi/projects/myapp/workflows/default.yaml`, `name: default`).
- **Doc claim:** The `default` workflow has five statuses (`backlog`, `todo`,
  `in-progress`, `review`, `done`) and two active `transitions`
  (`in-progress → review [push_branch, open_pr]`,
  `review → done [merge, delete_branch]`).
- **Contradicting source:**
  - `crates/shelbi-core/src/workflow.rs:665-732` (`default_workflow`) ships
    **six** statuses — the example omits `canceled` (category `archived`,
    `workflow.rs:710-717`).
  - `crates/shelbi-core/src/workflow.rs:719-726` — the default workflow ships
    `initial_status: None` and `transitions: None` (no transitions at all),
    deliberately lean. The scaffolded `default.yaml` writes transitions only
    as a *commented* optional block (`scaffold.rs:272-288`,
    `scaffold.rs:339-345`; test `scaffold.rs:537-542` asserts
    `wf.transitions.is_none()` as written).
- **Impact:** A reader treating the example as the real `default.yaml`
  believes the default flow is five statuses with active PR/merge automation.
  The shipped default is six statuses with no transitions (a pure
  status-track). Illustrative intent is plausible, but the file header and
  `name: default` present it as authoritative.

## F5 — statuses.mdx example omits the shipped `canceled` status

- **Severity:** medium
- **Type:** drift
- **Doc file + lines:** `statuses.mdx:25-33` (Example, headed
  `# ~/.shelbi/projects/myapp/workflows/statuses.yaml`).
- **Doc claim:** The scaffolded `statuses.yaml` catalog is five statuses:
  `backlog`/`todo`/`in-progress`/`review`/`done`.
- **Contradicting source:**
  `crates/shelbi-core/src/statuses.rs:173-206` (`default_project_statuses`)
  ships **six**, adding `{ id: canceled, name: Canceled, category: archived }`
  (`statuses.rs:201-205`). `scaffold.rs:620-632` asserts the scaffolded
  file equals that six-entry set.
- **Impact:** The example that claims to show the file `shelbi init` writes is
  missing an entry, and specifically the only `archived`-category status —
  the very category the page's own terminal-state rules
  (`statuses.mdx:63-64`) call out.

## F6 — statuses.mdx says repeating a category is fine, using `active` as the example — but duplicate `active` warns

- **Severity:** low
- **Type:** drift-risk (internally contradictory prose)
- **Doc file + lines:** `statuses.mdx:70-73`.
- **Doc claim:** "A category set is otherwise unconstrained: repeating a
  category is allowed (a long pipeline might have several `active`
  statuses), though a missing `handoff` or a duplicated single-instance
  category raises a non-fatal warning."
- **Contradicting source:**
  `crates/shelbi-core/src/statuses.rs:128-141` (`category_warnings`) emits a
  non-fatal warning when **any** of `backlog`/`ready`/`active`/`handoff`
  appears more than once. `active` is a single-instance category, so the
  parenthetical example ("several `active` statuses") is exactly the case the
  next clause says warns. Only `done` / `archived` may repeat without a
  warning.
- **Impact:** The illustrative example contradicts the rule stated one line
  later; a reader can't tell whether duplicate `active` is clean or warned
  (it warns).

## F7 — `git.branch_prefix` is an accepted key but is undocumented

- **Severity:** low
- **Type:** drift-risk (valid key omitted from the reference)
- **Doc file + lines:** `project.mdx:310-315` (git field table lists only
  `base_branch`, `branch`, `merge_strategy`); `workflow.mdx:257-261` (same
  three).
- **Doc claim (by omission):** The git block's branch-naming key is `branch`.
- **Contradicting source:**
  `crates/shelbi-core/src/model.rs:543-549` defines `branch_prefix` as a
  real, deserialized key (generates `<branch_prefix>/<task-id>`), mutually
  exclusive with `branch`
  (`model.rs:557-569` `validate_branch_exclusivity`; tested at
  `model.rs:4619-4642`). The project reference page never mentions it.
- **Impact:** A user with a `branch_prefix:` in their config (or hitting the
  `branch` + `branch_prefix` mutual-exclusion error) finds no documentation
  for the key. Low because `branch` is the documented, preferred form.

## F8 — machines `forward` field is undocumented

- **Severity:** low
- **Type:** drift-risk (valid key omitted)
- **Doc file + lines:** `project.mdx:100-106` (machines field table lists
  `name`, `kind`, `work_dir`, `host`, `tags`).
- **Doc claim (by omission):** A machine has five fields.
- **Contradicting source:**
  `crates/shelbi-core/src/model.rs:1031-1040` — `Machine` also has
  `forward: Option<ForwardMode>` (`unix` | `tcp`), controlling the hub's
  reverse-socket forward mode to an SSH machine.
- **Impact:** Minor. An advanced SSH-networking knob exists but isn't in the
  reference; most users never need it (default is auto-detect).

---

## Confirmed spot-checks (no finding — documented values verified correct)

These doc claims were checked and match the source; recorded so a fix pass
doesn't re-open them:

- `config.yaml` `keymap.zen_toggle` values `alt-z` / `ctrl-backslash` /
  `ctrl-g` / `ctrl-shift-z` / `none` and default `alt-z`
  (`user_config.rs:57-64`, `46-49`). — `global.mdx:32,50-56`
- `keys.yaml` leaf value forms (scalar / list / `[]` unbind / `null`
  fall-through) — `loader.rs:238-256`. — `global.mdx:93-98`
- Transition action set of exactly six (`push_branch`, `open_pr`, `merge`,
  `close_pr`, `delete_branch`, `restack`) with those wire names —
  `workflow.rs:1310-1339`. — `workflow.mdx:107-116`
- `ready_timeout` default `90` s — `workflow.rs:1297`. — `workflow.mdx:103,140`
- `review:` block fields (`workdir`/`setup`/`serve`/`ready`/`url`, `serve`
  required) — `workflow.rs:1434-1461`. — `workflow.mdx:198-217`
- `required_params` must list every `{{var}}` in `git.base_branch` or the
  workflow is rejected — `workflow.rs:472-482`. — `workflow.mdx:55,229-248`
- `zen.ci_timeout` default `900` s (15m); `zen.checks.local` default `[]`;
  `danger_paths` extend/override/bare-list shapes —
  `model.rs:2038-2069,2058-2060,2071-2131`. — `project.mdx:245-262`
- `merge_strategy` values `squash`/`merge`/`rebase`, default `squash`
  (snake_case wire form) — `model.rs:575-589`. — `project.mdx:314`,
  `workflow.mdx:261`
- `default_branch` default `main`; `workspace_poll_interval_secs` default `5`;
  `workspace_permissions_mode` default `auto` — `model.rs:509,619-624`.
  — `project.mdx:80,90,91`
- `config_mode` (`global` | `in-repo`, kebab-case, elided when `global`)
  — `model.rs:85-86,167-173`. — `project.mdx:89`
- Status `agent` derivation (`developer` for `active`, `orchestrator` for
  `ready`) — `workflow.rs:1168-1176` (`DEFAULT_ACTIVE_AGENT` /
  `DEFAULT_READY_AGENT`). — `workflow.mdx:75`
- `initial_status` default = first status; must reference a declared status
  — `workflow.rs:184-193,408-413`. — `workflow.mdx:53`
- Six status categories (`backlog`/`ready`/`active`/`handoff`/`done`/
  `archived`) and "load fails with no `done`/`archived` terminal" —
  `statuses.rs:88-98`, `workflow.rs:1088-1118`. — `statuses.mdx:51-74`
- In-repo config-root = `<repo>/.shelbi`; `workflows/statuses.yaml` under it
  — `project_paths.rs:104-126`. — `statuses.mdx:14-22`, `workflow.mdx:14-24`
- Heartbeat *shapes* (bare duration / `off` / map) and bare-integer rejection
  — `model.rs:373-406,468-503`. — `project.mdx:281-298` (only the *defaults*
  are wrong; see F2)

---

## Summary

| Severity | Count | Findings |
| --- | --- | --- |
| Critical | 0 | — |
| High | 3 | F1 (local.yaml split), F2 (heartbeat defaults), F3 (`review` keymap mode) |
| Medium | 2 | F4 (default.yaml example), F5 (statuses.yaml example) |
| Low | 3 | F6 (duplicate-`active` warning), F7 (`branch_prefix`), F8 (machine `forward`) |
| **Total** | **8** | |

Confirmed errors: F1, F2, F3 (behavior/schema flatly contradicts the doc).
Drift / drift-risk: F4, F5, F6, F7, F8 (examples or omissions that mislead but
don't state an outright-false fact about a documented key).

## Verification commands / paths

```
# Build (schema must compile before trusting struct reads)
cargo build --workspace

# Docs audited
site/content/docs/configuration/{global,project,statuses,workflow}.mdx

# Ground-truth source
crates/shelbi-core/src/scaffold.rs            # what `shelbi init` writes
crates/shelbi-core/src/model.rs               # Project/Git/Zen/Machine/Workspace/Heartbeat structs + defaults
crates/shelbi-core/src/workflow.rs            # Workflow/Status/Transition/ReviewServe, default_workflow()
crates/shelbi-core/src/statuses.rs            # categories, default_project_statuses()
crates/shelbi-cli/src/commands/init.rs        # render_project_yaml / write_in_repo_config / run_pick_up
crates/shelbi-cli/src/commands/workspace.rs   # save_workspace_config (writes local.yaml on edit)
crates/shelbi-state/src/lib.rs                # load_project / load_project_split precedence
crates/shelbi-state/src/migrate.rs            # the only path that writes the split local.yaml
crates/shelbi-state/src/project_paths.rs      # config_root resolution (global vs in-repo)
crates/shelbi-state/src/user_config.rs        # UserConfig / ZenToggleChord
crates/shelbi-state/src/keymap/loader.rs      # Keymaps modes, keys.yaml schema
crates/shelbi-state/src/keymap/actions.rs     # mode + action wire names

# Prior audit (dedupe / re-verify)
docs/documentation-codebase-audit.md          # 2026-07-13, finding H1 == F1 here
```
