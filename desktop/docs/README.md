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

The plan's Phase 7 also rewrites the product docs in the ContextStore
(`Product/vision.md`, `principles.md`, `positioning.md`) to describe one
backend with two clients, and the public site changes ship with the release;
both are separate tasks.
