# dt-figma-terminal-screens: the terminal screens match their Figma frames

## What landed

The Orchestrator, Workspace and Review main areas and the idle Workspace
screen now follow their Figma frames (Orchestrator 2160:1307 / 2220:20936,
Workspace 2165:186 / 2220:21038, Review 2165:275 / 2220:21066, Workspace idle
2174:851 / 2220:21583), in both themes. Values came from the Figma MCP
(`get_metadata`, `get_design_context`, `get_variable_defs` on the frames and
their main-content nodes 2164:173, 2172:991, 2168:679, 2174:855).

- **Insets** (`views/session_view.rs`). The grid sits 16px in from the main
  area's leading and trailing edges and 12px from its top and bottom
  (`TERMINAL_INSET_X/Y`, was 12 / 8): the Orchestrator's main content
  (`px 16, py 12`) and the Workspace's agent terminal (`px 16, py 12`). The
  Review frame's reviewer terminal is `p 12` after its 1px divider, so the
  review pane sets `REVIEW_TERMINAL_INSET_X` (12) through a new
  `SessionView::set_inset_x`. A trailing inset is new (the grid used to run
  to the edge).
- **Terminal colors** (`theme/mod.rs`, `theme/palette.rs`,
  `elements/terminal/layout.rs`). The frames bind their transcript lines to
  variables: default text `theme.foreground`, bold lines `theme.white`,
  tool output and footers `theme.muted`, added/check lines `theme.green`,
  the removed line `theme.red`, notices `theme.cyan`. So:
  - ANSI red (slot 1) is the new `theme_red` role, `theme.red`
    (`#e04f52` / `#c23b3e`); bright red keeps Tomorrow Night / GitHub light.
    Green and cyan already were `theme.green` / `theme.cyan`
    (`dt-figma-theme`). The task named `text/green` (`#31d924`) for green;
    the Workspace and Review frames bind `theme.green` (`#5acd25`), so that
    stays.
  - Bold text in the default foreground draws in `text_loud`
    (`TerminalPalette::bold_foreground`, `BrightForeground`); bold colored
    text keeps its color (no bold-is-bright for the ANSI eight).
  - Dim (SGR 2) text in the default foreground draws in `muted`
    (`dim_foreground`, `DimForeground`): exactly `#777a86` in dark (the old
    60% alpha landed at `#777a83`) and `muted` over white in light
    (`#858a9b`; the old fade was a neutral `#959595`). Dim colored text keeps
    its hue at 60% alpha. A program's OSC 10 foreground also colors its bold
    and dim text.
- **The idle Workspace page** (`views/workspace.rs`, `app.rs`). An idle
  workspace (no task) no longer pushes the workspace panel: the nav sidebar
  stays, as the frame draws it. `WorkspaceView::wants_panel(row)` (a task on
  the row) decides, in `sync_column`, in `perform(ShowView)` (no hold for a
  push that won't run) and in `finish_switch`. The main area shows the page
  (2174:996 / 2174:1007): a toolbar (`px 16, py 8, gap 4`; a 28px row of
  `· idle` Base muted and the name Base Medium loud, then the Shell button;
  `hub · ~/…/wt/charlie` X-Small muted), a 1px `divider`, then the agent
  pane, which shows the shared idle placeholder (`p 16`, 2px between lines,
  the name Base Medium loud, the rest Base muted). When a dispatch gives the
  workspace a task, the panel pushes in and the toolbar goes; when the task
  ends, the panel leaves. Keyboard focus goes to the agent pane's view (as
  the chat's) and the sidebar chord moves it to the sidebar.
- **The Shell button** opens and closes the workspace's user shell beside
  the placeholder (the same `toggle_shell` as the panel's Shell row; the
  button holds the `background` fill while the shell is open). The TUI's
  idle workspace has no shell affordance; the shell itself is the shared
  `EnsureShell` / `CloseShell` path, so nothing new crosses the client
  boundary (no `CommandKind`, no new `shelbi_app::view` field).
- **The working Workspace frame's top rule.** 2165:186 draws a 1px
  `theme.divider` across the top of the main area (2167:653); the main area
  draws it too, above the panes, in both states.
- **Harness.** `--visual-figma`'s idle frame shows charlie with no task (the
  other frames' sidebar has charlie awaiting input on a task: the mockups
  disagree, and the idle page is a task-less workspace's), and checks that no
  panel pushes in.

## Deliberate differences

- **Icon size.** The Shell button's command-line icon is the app's 20px
  Heroicon (the one-size rule, `assets.rs`), not Figma's 14px; the button is
  24px tall like the frame's, with 6px padding and gap.
- **Off-grid transcript spacing.** The Orchestrator frame draws Codex's
  header as a rectangle with 26px line spacing and puts 24px / 30px gaps
  between some lines; a terminal grid is 18px rows, so the transcript below
  the box sits 6px higher than drawn. That is the fixture's terminal output,
  not chrome.
- **The working Workspace frame's empty bottom 64px** (its panes frame ends
  at 960) has no counterpart: the panes fill the main area, and "Working…"
  sits where the fixture writes it.
- **The sidebar** is untouched (the render's column is 256px plus its handle
  where Figma's is 280px; `dt-figma-sidebar` / `dt-figma-task-panels`).

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test` (435 passed, rebased on `ea0041ed`) and `cargo clippy --all-targets --features
  visual-check -- -D warnings`. No `crates/` change, so no root workspace run.
- New and updated tests: `an_idle_workspace_shows_its_page_without_the_panel`
  (toolbar 64px tall as drawn, the rule, focus, the Shell button's
  `EnsureShell` / `CloseShell`, the panel once a task arrives),
  `an_idle_workspace_keeps_the_sidebar_and_a_task_pushes_the_panel` (window
  level: no push, no hold, the push on dispatch),
  `the_idle_toolbar_writes_home_as_a_tilde`, `bold_default_text_is_the_loud_color`,
  `dim_and_hidden` (dim default = `#777a86`, dim red keeps its hue),
  `terminal_palette_follows_the_theme` (red, bold and dim targets, light
  green / cyan / red), `terminal_text_stays_legible_on_both_surfaces` (bold
  foreground at AA, dim foreground at 3:1 alongside the hues), and the
  Workspace layout tests (the terminal under the 1px rule).
- `--visual-figma` in both themes: 0 checks failed (with the new
  workspace-idle check). Each of the four screens looked at beside its
  reference in both themes; the transcript lines' colors sampled
  (`magick … -format %c histogram:info:-`): the removed line is `#e04f52` /
  `#c23b3e` in render and reference alike.
- Main-area diff (the reference's main content from x=280, 281 for Review;
  the render's from x=257 where its narrower column ends; largest channel
  difference over 10%, as `figma-diff.sh` counts):

  | screen | before dark | after dark | before light | after light |
  |---|---:|---:|---:|---:|
  | orchestrator | 3.87% | 3.37% | 3.90% | 3.45% |
  | workspace | 3.16% | 0.98% | 3.17% | 1.00% |
  | review | 2.24% | 1.15% | 2.28% | 1.19% |
  | workspace-idle | 0.69% | 0.38% | 0.74% | 0.44% |

  What is left in the Orchestrator is the off-grid spacing above; in the
  Workspace, "Working…"'s row and the transcript's dim bullets (the fixture
  paints `•` dim, the frame draws it in the line's color).
- `figma-diff.sh` over the whole frames (dominated by the sidebar's width,
  which shifts everything 23px): orchestrator 5.89 → 5.79% dark, 4.68 →
  4.60% light; workspace 3.69 → 3.39%, 3.68 → 3.41%; review 3.86 → 3.63%,
  3.44 → 3.21%; workspace-idle 1.65 → 3.18% dark, 1.69 → 1.97% light (the
  idle frame now draws the nav sidebar, as the reference does, so the
  sidebar's own differences enter the count; the main area improved, above).
