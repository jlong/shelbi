# dt-figma-machines: the Machines view to the Figma frame

Status: done, 2026-10-18. Frames: Machines dark `2165:456`, light
`2220:21211`. Values pulled with the Figma MCP (`get_metadata`,
`get_design_context` on `2165:544`, `get_variable_defs` on both frames), not
measured from the PNGs.

## What the frame specifies, and what the view now draws

| Piece | Figma | View |
|---|---|---|
| Toolbar | 37 high, 16 side padding, gap 8: "Machines" Base Medium `theme.white`, the project X-Small `theme.muted` | same; a 1px `theme.divider` under it |
| Body | 16 side, 8 top/bottom padding; machines 16 apart; each machine a column, gap 2 | same (`px_4 py_2 gap_4`, `gap(2)`) |
| Machine heading | 24 high, gap 12: name Base Medium `theme.white`, `local` / `ssh <host>` X-Small `theme.muted`, then `● reachable` X-Small `theme.green` | same; checking is muted, unreachable red with the reason cut at 80 and the whole reason in a tooltip (unchanged behavior) |
| Remote metadata | 24 high, gap 8, X-Small: `[gpu, linux]` in `theme.workflow_badge_fg`, `/usr/local/bin/shelbi v0.10.0` muted | same; an incompatible binary stays red, "not set up" stays the warning color. Shown for a remote, or a local machine with tags |
| Workspace row | 30 high, 16 left / 8 right padding, radius 6, gap 12; Base Regular | same |
| Columns | name 150 `theme.white`; state 76 (working `theme.green`, review `theme.cyan`, idle muted); runner 96 `theme.gray`; task flex `theme.gray`; branch X-Small `theme.ansi_dark_gray` with `⎇` | same. Other states keep their meaning colors (awaiting yellow, blocked red, paused magenta) |
| Selected row | `theme.selection` band, no marker | same; the accent pill and card shadow are gone. Unselected rows keep a hover fill (`theme.background`) the frame doesn't draw |
| Footer | 1px divider, then 26 high, 16 side padding, X-Small muted: hints left, "N workspaces" right | same |

The cards (`theme.card` + raised shadow) and chips for states, tags and
reachability are gone: the frame draws plain text on the canvas.

## Model changes (shared)

- `MachineWorkspace.runner` (+ `MachinesInput.runners`): the agent CLI a
  task's agent launches on. `shelbi_orchestrator::machines_model` resolves it
  through `workspace::resolve_workspace_launch`, the seam dispatch uses (task
  `launch:` > project `agents.<name>.runner` > the agent's `preferred_runner` >
  the fleet's launcher for that kind), for in-progress tasks (their `agent:`,
  default `developer`) and review slots (`review`). Labels: `Claude`, `Codex`,
  else the generic runner's command basename.
- `MachineWorkspace::task_cell()`: `in_progress: <title>` / `review: <title>`,
  falling back to the ids cell when the board has no title or a diverged
  board assigns several tasks. The desktop shows the ids cell as a tooltip.
- The TUI is unchanged (it keeps its agent column and ids + title); the
  parity doc says so for `runner`, `runners`, `agent`.

## Decisions

- **Idle rows show `—` for the runner.** The frame is inconsistent: charlie
  (idle) shows `—`, gpu-1 (idle) shows `Codex`. No runner is resolved without
  a task (the status file records none), so both read `—`.
- **A serving review slot's state reads `review`**, as drawn, instead of the
  shared badge's `serving`. A slot whose server is still starting shows
  `starting` (muted) before the branch, and a review URL stays a clickable
  link there; a serving slot with no URL adds nothing.
- **Contrast.** The dark selection band (`#acb4cf8c` over the canvas) is
  light, so every cell on the selected row falls short: gray 2.12, green
  2.30, cyan 1.60, muted 1.11 (an idle row selected, which the frame doesn't
  draw), dark gray 1.13. Light misses narrowly (3.9 to 4.1). These are drawn
  as Figma specifies and listed as exceptions in `theme.md`. **Flagged for
  John**: the dark selected row is hard to read, worst on a selected review
  row's cyan state.
- The contrast test now checks `theme_green` / `theme_cyan` on the canvas and
  each Machines cell color on the selection band.

## Fixture (`--visual-figma`)

The project now declares a `claude` runner and pins `developer` to Codex and
`review` to Claude, and the review slot writes its serving marker, so the
rows read "Codex / Codex / — / Claude" with `review` in the state column, as
in the frame.

## Verification

- `--visual-figma` render, both themes, compared side by side with
  `machines-{dark,light}.png` (crops of the main area): toolbar, headings,
  row baselines and column x positions (name 312, state 474, runner 562, task
  670 in the frame; the render is the same 23px left with the narrower
  sidebar) and footer line up with the frame.
- `figma-diff.sh`: machines dark **22.17% -> 3.89%**, light **6.06% ->
  2.73%** (post-theme numbers from `dt-figma-theme.md`). The remaining
  regions are all in the sidebar (`dt-figma-sidebar`'s), plus content the
  fixture differs on: the mockup's shorter task titles ("Add project
  switching" vs. the fixture's "Add project switching to the desktop app"),
  the header's project name, and the sidebar's 257px width shifting the main
  area 23px left.
- `cargo build`, `cargo clippy --all-targets -- -D warnings` (with and without
  `visual-check`), `cargo test` (449 passed after the rebases) in `desktop/`.
- Root: `cargo clippy -p shelbi-app -p shelbi-orchestrator -p shelbi-tui
  --all-targets -- -D warnings`; `shelbi-app` machines + `parity_doc` tests,
  `shelbi-tui` machines tests, the new `machines_model` runner-label test.
  `Cargo.lock` unchanged.
- Selection, open and refresh: the state machine (`machines_state.rs`) is
  untouched and its tests pass; the click handler and key handler are the
  same; the desktop smoke test runs every registry command.
- Not checked live: an unreachable remote (the harness stubs every remote
  reachable). Its label and tooltip run through the same code as before,
  restyled from a chip to text.

## Rebases onto the merged `dt-figma-*` siblings (2026-10-18)

Rebased onto `jlong/desktop-app` after `dt-figma-activity` merged, then
again as `dt-figma-palette`, `dt-figma-issues-board`, `dt-terminal-font-zoom`,
`dt-figma-terminal-screens` and `dt-figma-settings` merged: their
`docs/README.md` index rows conflicted (all rows kept), and Settings'
`form_text` contrast checks sat beside this task's selection-band checks in
`palette.rs` (both kept). The
conflicts in `docs/README.md`, `theme.md` and `theme/palette.rs` keep both
sides: both index rows, the Activity `log_text` exception row beside this
task's `muted` / `dark_gray` rows (their worst ratios, 1.11 and 1.13, still
come from the selected Machines row), and both sets of contrast checks.

Each branch had removed one of the last two callers of `Theme::tone_hue`
(Activity's tag chips, Machines' badge chips), so after the merge it and the
`Hue::Cyan` / `Hue::Magenta` variants it alone produced were dead code
(clippy `-D warnings` failed). Removed them; the `cyan_tint` /
`magenta_tint` palette roles stay (documented, contrast-checked).

Re-checked after the last rebase (onto `dt-figma-settings`): desktop
`cargo build`, `cargo clippy --all-targets -- -D warnings` with and without
`visual-check`, `cargo test` (449 passed); root `cargo clippy --workspace
--all-targets -- -D warnings`. `--visual-figma` + `figma-diff.sh`: machines
dark **3.89%**, light **2.73%**, unchanged through every rebase.
