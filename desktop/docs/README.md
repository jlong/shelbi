# Desktop app docs

The in-repo map of the desktop app effort: the plan's phases, the `dt-*`
subtasks that built each piece, and where each one's record lives. The full
design is the ContextStore plan "Desktop App (gpui)"
(`Plans/desktop-app-gpui.md`); how to build, run, configure, and release the
app is in [`../README.md`](../README.md).

All work landed on the long-lived branch `jlong/desktop-app` first. The
foundation (the umbrella, task `desktop-app`) set up the separate workspace,
the window, and the exhaustive dispatch with a not-implemented stub; each
`dt-*` subtask branched from and squash-merged back into it and replaced the
stub arms it implemented. `dt-parity-audit` removed the stub for good.

## Reference

| Doc | What it is |
|---|---|
| [`parity.md`](parity.md) | TUI parity: where each client shows every view-model field (guarded by a test), and the hand walk of every command in both clients |
| [`theme.md`](theme.md) | The desktop's palette (the Figma tokens as roles, dark and light), its type styles, the terminal font, its contrast rules and exceptions, and the shared component shapes |
| [`gpui.md`](gpui.md) | The gpui channel: what is pinned and why, the breakage log, gpui quirks met along the way |
| [`status/desktop-app.md`](status/desktop-app.md) | The umbrella: foundation, and the record of the whole effort |

## Status docs by phase

Each subtask's status doc says what landed, what was decided and why, what
is left, and how it was verified.

### Phase 0: Spike

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-spike-terminal` | The terminal element on gpui; the gpui channel decision | [`status/dt-spike-terminal.md`](status/dt-spike-terminal.md) |

### Phase 1: App shell

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-platform-bootstrap` | Finding the CLI from a bare launch, starting the daemon, the version check and relaunch prompt | [`status/dt-platform-bootstrap.md`](status/dt-platform-bootstrap.md) |
| `dt-theme-tokens` | One set of named color tokens, dark and light, shared with the TUI | [`status/dt-theme-tokens.md`](status/dt-theme-tokens.md) |
| `dt-shell-sidebar` | The window, the sidebar, project switching, the refresh pipeline, settings | [`status/dt-shell-sidebar.md`](status/dt-shell-sidebar.md) |
| `dt-chat-view` | The orchestrator chat | [`status/dt-chat-view.md`](status/dt-chat-view.md) |

### Phase 2: Terminal element to completion

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-terminal-complete` | Scrolling, selection, find, links, keys, mouse, fonts, accessibility | [`status/dt-terminal-complete.md`](status/dt-terminal-complete.md) |
| `dt-workspace-view` | The Workspace view: the agent, a user shell beside it, remote reconnect | [`status/dt-workspace-view.md`](status/dt-workspace-view.md) |
| `dt-terminal-perf-multiclient` | The terminal bench, concurrent clients on one session, idle CPU | [`status/dt-terminal-perf-multiclient.md`](status/dt-terminal-perf-multiclient.md) |

### Phase 3: Issues, task detail, machines

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-issues-board` | The Issues board and task detail | [`status/dt-issues-board.md`](status/dt-issues-board.md) |
| `dt-board-dnd` | Drag and drop on the board, reordering within a column | [`status/dt-board-dnd.md`](status/dt-board-dnd.md) |
| `dt-activity-machines` | The Activity feed and the Machines view | [`status/dt-activity-machines.md`](status/dt-activity-machines.md) |

### Phase 4: Activity, palette, overlays

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-palette-overlays` | The command palette and the overlays (error log, add project, load for review, reject, Zen intro) | [`status/dt-palette-overlays.md`](status/dt-palette-overlays.md) |
| `dt-menus-notifications` | Native menus with `keys.yaml` key equivalents, system notifications | [`status/dt-menus-notifications.md`](status/dt-menus-notifications.md) |

### Phase 5: Review

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-review-view` | The Review view: the panel beside the reviewer, diff and editor sessions, approve and reject, live server state | [`status/dt-review-view.md`](status/dt-review-view.md) |

### Phase 6: Packaging

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-packaging` | A signed, notarized `Shelbi.app`, its release job, the Homebrew cask, the update notice | [`status/dt-packaging.md`](status/dt-packaging.md) |

### Phase 7: Parity audit and release

| Subtask | Scope | Status doc |
|---|---|---|
| `dt-parity-audit` | No stubs left; scenario tests for every command; a smoke test per front end; the field checklist; the hand walk; these docs | [`status/dt-parity-audit.md`](status/dt-parity-audit.md) |
| `dt-merge-main` | `origin/main` merged into the umbrella: the four conflicts resolved, main's `orchestrator_status` shown in both clients | [`status/dt-merge-main.md`](status/dt-merge-main.md) |
| `dt-icon-sizes` | Every icon at 24, 20 or 16px (superseded by `dt-icons-mini-outline`) | [`status/dt-icon-sizes.md`](status/dt-icon-sizes.md) |
| `dt-icons-mini-outline` | Every icon is the Heroicons outline drawing at 20×20, through one size-free helper | [`status/dt-icons-mini-outline.md`](status/dt-icons-mini-outline.md) |
| `dt-terminal-bottom-clipped` | A pane always reaches the session's last row: clipped windows keep the content's bottom, and taking focus takes the PTY's size | [`status/dt-terminal-bottom-clipped.md`](status/dt-terminal-bottom-clipped.md) |
| `dt-workspace-sidebar` | The workspace panel takes the sidebar's column with the review's push; the header above the agent is gone | [`status/dt-workspace-sidebar.md`](status/dt-workspace-sidebar.md) |
| `dt-instant-panel-push` | The panel's push starts on the input's next frame (before the review load answers) and its session attaches once it settles | [`status/dt-instant-panel-push.md`](status/dt-instant-panel-push.md) |
| `dt-board-stale-after-own-mutation` | A card's second change goes right after its first: the daemon's reply carries the state the change landed at, and the board sends the next one against it | [`status/dt-board-stale-after-own-mutation.md`](status/dt-board-stale-after-own-mutation.md) |
| `dt-switch-project-in-window` | A project switch retargets the current window in place, restoring where the project was left; File ▸ New Window (⌘⇧N) opens more; window frames per slot, restored at launch | [`status/dt-switch-project-in-window.md`](status/dt-switch-project-in-window.md) |
| `dt-figma-visual-harness` | `--visual-figma` renders the ten Figma mockup screens in both themes over the mockups' content; `scripts/figma-diff.sh` diffs them against the reference PNGs; the baseline report | [`status/dt-figma-visual-harness.md`](status/dt-figma-visual-harness.md) |
| `dt-taskpage-theme` | The desktop's own palette and component shapes after the Taskpage design system, light and dark (superseded by `dt-figma-theme`) | [`theme.md`](theme.md) |
| `dt-figma-theme` | The Figma design system's palette, type styles and Source Code Pro terminal font replace Taskpage's, in both themes | [`status/dt-figma-theme.md`](status/dt-figma-theme.md) |
| `dt-figma-activity` | The Activity view restyled to the Figma frame in both themes: header with the display name, the frame's filter controls, day groups, rows colored by actor kind and status, current-status tags, the "available" and "started" desktop labels | [`status/dt-figma-activity.md`](status/dt-figma-activity.md) |
| `dt-figma-palette` | The command palette restyled to the Figma Command palette frame in both themes: geometry, scrim, query line, rows, description column, Projects column, footer; the frame's default list and copy; Tab crosses into the Projects column | [`status/dt-figma-palette.md`](status/dt-figma-palette.md) |
| `dt-figma-issues-board` | The Issues board restyled to the Figma Issues frame: filter toolbar, status-glyph column headers, cards with excerpts and workflow badges, the key-hint footer; the shared status glyphs | [`status/dt-figma-issues-board.md`](status/dt-figma-issues-board.md) |
| `dt-terminal-font-zoom` | ⌘+ / ⌘− / ⌘0 (View ▸ Bigger, Smaller, Actual Size) step every terminal's font size and save it to the setting Settings edits | [`status/dt-terminal-font-zoom.md`](status/dt-terminal-font-zoom.md) |
| `dt-figma-terminal-screens` | The Orchestrator, Workspace and Review terminals take the Figma frames' insets and colors (bold loud, dim muted, `theme.red`); an idle workspace keeps the sidebar and shows the Figma idle page with a Shell button | [`status/dt-figma-terminal-screens.md`](status/dt-figma-terminal-screens.md) |
| `dt-figma-settings` | Settings restyled to the Figma frame in both themes; a terminal font picker (bundled faces plus installed monospaced families, each previewed in itself) and a size stepper (8 to 32) replace the free-text fields | [`status/dt-figma-settings.md`](status/dt-figma-settings.md) |
| `dt-figma-machines` | The Machines view drawn to the Figma Machines frame in both themes: plain headings, a remote's metadata line, flat rows with the runner and `<status>: <title>`, the shortcuts footer | [`status/dt-figma-machines.md`](status/dt-figma-machines.md) |
| `dt-figma-sidebar` | The sidebar is the Figma Navigation component's Default state: its sections and rows, the Changelog item (the Activity view, desktop-only label), ⌘O / ⌘I / ⌘L and ⌘1-9, the traffic lights' position | [`status/dt-figma-sidebar.md`](status/dt-figma-sidebar.md) |
| `dt-figma-issue-detail` | The issue detail (the card's modal over the board) restyled to the Figma Issue detail frame in both themes: header, action buttons, metadata, body, checklist | [`status/dt-figma-issue-detail.md`](status/dt-figma-issue-detail.md) |
| `dt-figma-task-panels` | The Workspace and Review panels are the Navigation component's Workspace and Review states, inside the sidebar's chrome | [`status/dt-figma-task-panels.md`](status/dt-figma-task-panels.md) |
| `dt-figma-qa-palette` | Pixel QA of the command palette: the drop's blur as gpui's sigma, the scrim, row and footer metrics | [`status/dt-figma-qa-palette.md`](status/dt-figma-qa-palette.md) |
| `dt-figma-qa-activity` | Pixel QA of Activity: 14pt picker icons (the small Heroicons), title wrapping; found Figma's text set in Geist 1.2 | [`status/dt-figma-qa-activity.md`](status/dt-figma-qa-activity.md) |
| `dt-figma-qa-settings` | Pixel QA of Settings: the frame's 12pt check, field icons in `form_text`, the copy | [`status/dt-figma-qa-settings.md`](status/dt-figma-qa-settings.md) |
| `dt-figma-qa-issues-board` | Pixel QA of the Issues board: focus ring and glow, 18pt button icons (`button_icon` role), card shadows | [`status/dt-figma-qa-issues-board.md`](status/dt-figma-qa-issues-board.md) |
| `dt-figma-qa-machines` | Pixel QA of Machines: the frame's gray as the `table_text` role, the undrawn states rendered | [`status/dt-figma-qa-machines.md`](status/dt-figma-qa-machines.md) |
| `dt-figma-qa-terminal-screens` | Pixel QA of the Orchestrator, Workspace, Review and idle terminals: crisp box-drawing lines, the idle page | [`status/dt-figma-qa-terminal-screens.md`](status/dt-figma-qa-terminal-screens.md) |
| `dt-figma-qa-sidebar` | Pixel QA of the sidebar: font smoothing off for the app (dark text weight), exact-width hotkey text | [`status/dt-figma-qa-sidebar.md`](status/dt-figma-qa-sidebar.md) |
| `dt-figma-qa-issue-detail` | Pixel QA of the issue detail: 18pt action icons, the modal's metrics and colors | [`status/dt-figma-qa-issue-detail.md`](status/dt-figma-qa-issue-detail.md) |
| `dt-figma-qa-task-panels` | Pixel QA of the task panels: a shaped line clamp, the 1pt column divider (the main area at x 281), the dark Success label | [`status/dt-figma-qa-task-panels.md`](status/dt-figma-qa-task-panels.md) |
| `dt-figma-qa-final` | Final pixel QA of all 20 frames at 1x and 2x, the real window in both themes with live theme switching, shared components across screens, the bundled fonts; the departures list for John | [`status/dt-figma-qa-final.md`](status/dt-figma-qa-final.md) |
| `dt-figma-semantic-token-sync` | The palette synced to Figma's `03 Desktop` collection: semantic token names, the new text, button-icon and Activity ANSI values, the checkbox's own border | [`status/dt-figma-semantic-token-sync.md`](status/dt-figma-semantic-token-sync.md) |
| `dt-figma-button-focus-colors` | Success and Danger buttons focus in their own green and pink (`focus/success/*`, `focus/danger/*`); the buttons sheet checks every style's ring and glow colors, focused over hover and press, and disabled | [`status/dt-figma-button-focus-colors.md`](status/dt-figma-button-focus-colors.md) |
| `dt-figma-button-size-content` | Explicit Button sizes (X-Small 24, Small 28, Base 32 with 14px text) and shared content (left icon, label, Hotkey, right icon) in the component's slots; icons in the style's icon role; disabled keeps its size and dims once; a `button-sizes` sheet | [`status/dt-figma-button-size-content.md`](status/dt-figma-button-size-content.md) |
| `dt-add-project-wizard` | Add Project is a wizard in its own 800 × 580 window (folder, then where settings live, then Create Project) from every entry point and the first launch; the one-form overlay is gone | [`status/dt-add-project-wizard.md`](status/dt-add-project-wizard.md) |

The plan's Phase 7 also rewrites the product docs in the ContextStore
(`Product/vision.md`, `principles.md`, `positioning.md`) to describe one
backend with two clients, and the public site changes ship with the release;
both are separate tasks.
