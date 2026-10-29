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
| `dt-sidebar-hover-stuck` | At most one sidebar row hovered and no blinking fills: every fade settled against the hit test each frame it paints, unpainted keys forgotten, sidebar rows keyed by row | [`status/dt-sidebar-hover-stuck.md`](status/dt-sidebar-hover-stuck.md) |
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
| `dt-project-creation-checklist` | Create Project shows screen 03: a checklist of the creation engine's stages (config, agents, workflows), a failure with Try Again that finishes the same project, then a created screen that waits for Open Project | [`status/dt-project-creation-checklist.md`](status/dt-project-creation-checklist.md) |
| `dt-wizard-workflow-summary` | Screen 04: the Task and Subtask workflows a new project was created with, then Okay re-reads the catalog and opens the project on its Orchestrator (a failed open stays on 04 and retries the open, never the creation) | [`status/dt-wizard-workflow-summary.md`](status/dt-wizard-workflow-summary.md) |
| `dt-workspace-settings-index` | Settings ▸ Workspaces (⌘5, before Workflows): the project's workspaces (worktree, machine, effective tags, effective slot) and the machines that host them (SSH host, root directory, count, Local or SSH reachability), read off the UI thread; its buttons ask for the workspace and machine forms through explicit intents | [`status/dt-workspace-settings-index.md`](status/dt-workspace-settings-index.md) |
| `dt-wizard-cli-setup` | The first launch's "Let's get Shelbi ready" screens (Check, Install, Ready) before the wizard: no CLI found installs the app's own release from GitHub into `~/bin` (checksum-verified, staged, rolled back on failure), then Ready opens Add Project or the saved windows | [`status/dt-wizard-cli-setup.md`](status/dt-wizard-cli-setup.md) |
| `dt-machine-settings-forms` | Add machine and Configure machine: a 600pt dialog over the window that edits one machine (Name and SSH host, Project directory as `work_dir`, Machine tags; This Mac's without SSH, with Choose…) and saves it through the shared machine service, refusals kept on the draft; the connection-check region hands readiness requests to the readiness service | [`status/dt-machine-settings-forms.md`](status/dt-machine-settings-forms.md) |
| `tui-settings-shell` | Settings in the TUI over the desktop's navigation, moved to `shelbi_app::settings` (pages offered per client, the leave prompt, stale-load tickets); `action:settings` opens it in both clients | [`status/tui-settings-shell.md`](status/tui-settings-shell.md) |
| `tui-settings-general` | The TUI's General (live theme, editor command) and Terminal (host-owned, swatches, preview) pages; narrow comment-preserving `update_user_config`; shared `settings::general` and `terminal_preview` | [`status/tui-settings-general.md`](status/tui-settings-general.md) |
| `dt-workspace-settings-forms` | New workspace and Edit workspace dialogs over Settings ▸ Workspaces (name, machine picker, own tags, derived worktree location, optional slot) through the shared `pool_admin` service, and the removal confirmation that never forces; modal keys, errors beside their fields, focus back on the row | [`status/dt-workspace-settings-forms.md`](status/dt-workspace-settings-forms.md) |
| `tui-settings-workflows` | Settings ▸ Workflows in the TUI over the desktop's index state, moved to `shelbi_app::settings::workflows` (per-row errors, a keyboard reorder step): the workflows and status catalog edited in place, saved off the input loop one at a time, deletes confirmed | [`status/tui-settings-workflows.md`](status/tui-settings-workflows.md) |
| `dt-machine-readiness-setup` | The machine forms' connection check made real: one bounded SSH round trip checks the Shelbi CLI, Git and the project directory (`machine_readiness`, shared with `shelbi machine check`), each failure told apart with its next step; Install CLI runs the shared remote setup and checks again; results bound to the draft and its request, a closed form's check killed, verified binaries recorded only for committed machines | [`status/dt-machine-readiness-setup.md`](status/dt-machine-readiness-setup.md) |
| `tui-settings-keybindings` | Settings ▸ Keybindings in the TUI, editing the same `keys.yaml` over the desktop's editor state (moved to `shelbi_app::settings::keybindings`); a saved change applies to the running TUI at once, and both clients now show a change only once it is written | [`status/tui-settings-keybindings.md`](status/tui-settings-keybindings.md) |
| `tui-settings-agents` | Settings ▸ Agents in the TUI over the desktop page's state, moved to `shelbi_app::settings::agents`: the agents table (stacked when narrow), New, Duplicate, guarded Reset and Delete, Edit YAML, and an in-place shared-instructions editor; saves queue off the UI thread and a project switch drops stale results | [`status/tui-settings-agents.md`](status/tui-settings-agents.md) |
| `tui-settings-workspaces` | Settings ▸ Workspaces in the TUI (5, before Workflows): the pool and its machines, with keyboard New / Edit workspace forms and a confirmed removal over the shared pool service; the desktop's workspaces and workspace-form state moved to `shelbi_app::settings`, `pool_admin`'s pure planners to `shelbi_core::pool` | [`status/tui-settings-workspaces.md`](status/tui-settings-workspaces.md) |
| `tui-settings-machine-forms` | The Add machine / Configure machine forms in the TUI (Settings ▸ Workspaces, `m` / Enter on a machine), over the shared machine service; the desktop's draft moved to `shelbi_app::settings::machine_form` with its connection check layered on top; drafts start Not checked and the probe is labeled as such | [`status/tui-settings-machine-forms.md`](status/tui-settings-machine-forms.md) |
| `tui-settings-machine-readiness` | The machine forms' connection check in the TUI, over the desktop's, now shared (`shelbi_app::settings::machine_check`: the check region, its stages, `CheckStop`, the readiness service type; `machine_readiness::{report, install_outcome, service}` behind it): Test connection checks the unsaved draft and lists SSH, Platform, Shelbi CLI, Git and Project directory with their results, Stop check, Install CLI (the shared setup, hub-copy fallback) then a recheck, all off the input loop; nothing saved until Add / Save, the verified binary recorded once saved; an install's side effects said even after Cancel | [`status/tui-settings-machine-readiness.md`](status/tui-settings-machine-readiness.md) |
| `tui-agent-editor` | The TUI's agent details editor over the desktop's draft, moved to `shelbi_app::settings::agent_editor` (shared choices, file targets, project-inherited values, the conflict flag): every field by keys, instructions edited in place or in the user's editor, skills added, imported by path, inspected and removed, Save off the input loop, Save / Discard / Cancel before leaving, switching project or opening a file | [`status/tui-agent-editor.md`](status/tui-agent-editor.md) |
| `tui-workflow-editor-form` | The TUI's workflow editor over the desktop's draft, moved to `shelbi_app::settings::workflow_editor` (a shared text form, the conflict flag): every section edited by keys, Save off the input loop, Save / Discard / Cancel before leaving or switching project, Reload or Keep editing on an on-disk conflict | [`status/tui-workflow-editor-form.md`](status/tui-workflow-editor-form.md) |
| `dt-feedback-dialog` | Contact Us: the sidebar footer's Settings and Send Feedback megaphone in the left corner; the megaphone and Help ▸ Send Feedback… open a modal (name, email, Bug Report / Feature Request / Other, message, system info) that posts to the 32pixels.co feedback endpoint off the UI thread, with validation, inline 422 / 429 / server errors, one send at a time, a thank-you, and the name and email remembered per user | [`status/dt-feedback-dialog.md`](status/dt-feedback-dialog.md) |
| `dt-contrast-setting` | Settings ▸ General's Contrast slider (0 to 100%, `desktop.contrast`): a resolve-time move of the palette's roles by class (surfaces darker and text, icons and rules brighter in Dark, the reverse in Light, hues kept), live in every window, composed with translucency; terminal colors untouched; no pairing loses contrast as it rises | [`status/dt-contrast-setting.md`](status/dt-contrast-setting.md) |
| `changelog-desktop` | The Changelog page in the sidebar's old Activity row (rocket, ⌘L): a blog-style column of entries newest first, Markdown descriptions, link chips (issue opens in Shelbi, PR and URL in the browser), search with highlights, entry-by-entry keys, paging as you scroll, live | [`status/changelog-desktop.md`](status/changelog-desktop.md) |
| `tui-workflow-editor-builder` | The workflow Builder in the TUI over the same draft as its Form (`v`, Builder \| Form): the desktop's graph and selection state moved to `shelbi_app::settings::{workflow_graph, workflow_builder}`; statuses as boxes, transitions routed cell by cell, one details panel (beside the canvas, or in its place when narrow), keyboard node and edge navigation, Add transition by keys or clicks with Esc dropping it, the Form's validation | [`status/tui-workflow-editor-builder.md`](status/tui-workflow-editor-builder.md) |
| `tui-stderr-corrupts-screen` | Nothing written over the TUI's frame: GitHub retry notices go to a per-process sink (the TUI's status band and `tui.log`; the desktop's log) instead of stderr, fd 2 points at `tui.log` while the alternate screen is up, and Ctrl+L (with focus main), a resize and focus regained repaint every cell | [`status/tui-stderr-corrupts-screen.md`](status/tui-stderr-corrupts-screen.md) |
| `tui-sidebar-footer-buttons` | Settings and Send Feedback (and the unread-errors badge) in the TUI footer's bottom-left, the desktop's order, with half-block hover pills and the desktop's tooltips; the shared `action:send-feedback` command runs the desktop's megaphone and Help ▸ Send Feedback… too | [`status/tui-sidebar-footer-buttons.md`](status/tui-sidebar-footer-buttons.md) |
| `tui-settings-textboxes` | Every TUI settings field as one half-block textbox like the sidebar search (label over a filled box, help or error under it; focus, invalid and read-only as fills, new `input*` theme tokens), multi-line fields in the same fill, a field's three rows its click target, the modal forms' footers pinned | [`status/tui-settings-textboxes.md`](status/tui-settings-textboxes.md) |
| `tui-settings-tables` | Every TUI settings table (workspaces, machines, workflows, statuses, agents, keybindings) from one widget: a header over a rule, a `│` between every cell, the selected row a half-block pill and the row under the pointer lit in the new `row_hover`, columns matched to the desktop's, cut and then dropped when narrow | [`status/tui-settings-tables.md`](status/tui-settings-tables.md) |
| `tui-feedback-dialog` | Contact Us in the TUI, from the footer's `📣` and the palette: the desktop's dialog in half blocks over the shared form, payload and sender (moved to `shelbi_app::feedback`); a TUI report's `appName` is "Shelbi TUI"; the name and email remembered in `~/.shelbi/feedback.json` | [`status/tui-feedback-dialog.md`](status/tui-feedback-dialog.md) |
| `tui-acknowledgements` | Acknowledgements in the TUI: a full-screen browser of the bundled software (search, list, license) over the desktop's model, moved to `shelbi_app::licenses` with the shared build-time generator; the TUI's list is what the `shelbi` binary links; `action:acknowledgements` in the palette, About's link and the desktop's Help menu | [`status/tui-acknowledgements.md`](status/tui-acknowledgements.md) |
| `tui-settings-polish` | Every TUI settings page, dialog and editor walked against the desktop: one half-block button (Secondary, Primary, Danger) with hover and focus, a segmented control, `▾` dropdowns, editor title bars, section headings with their buttons, dialogs on the palette's panel in the desktop's wording and button order; the findings checklist | [`status/tui-settings-polish.md`](status/tui-settings-polish.md) |
| `tui-footer-square-buttons` | The TUI footer's Settings, Send Feedback and unread-errors buttons as always-visible square half-block buttons, drawn by the back button's own helper (`panel::draw_square_button`); hover lifts the fill, and a hover name too long for beside the squares floats on the row above them | [`status/tui-footer-square-buttons.md`](status/tui-footer-square-buttons.md) |

The plan's Phase 7 also rewrites the product docs in the ContextStore
(`Product/vision.md`, `principles.md`, `positioning.md`) to describe one
backend with two clients, and the public site changes ship with the release;
both are separate tasks.
