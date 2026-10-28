# tui-settings-polish: every TUI settings screen next to the desktop's

## The change

A presentation pass over every Settings page, dialog and editor in the TUI,
walked side by side with `desktop/src/views/` (settings, settings_nav,
workspaces, machines, machine_form, workspace_dialog, workflows,
workflow_editor, workflow_builder, agents, agent_editor, keybindings,
font_picker). No setting, save or model behavior changed.

## One control language

- **Buttons** (`crates/shelbi-tui/src/button.rs`): the textbox's half-block
  shape (`▄` caps, label on a fill, `▀` caps), in the desktop's kinds:
  Secondary (gray), Primary (cyan tint), Danger (red tint). With the
  keyboard a button goes solid cyan (solid red for Danger), the desktop's
  focus-ring color; a disabled one is a read-only field's muted look, and
  shows a read-only field's cyan-tint focus when the keyboard lands on it.
  Rows of buttons wrap to another band instead of dropping one, so a narrow
  dialog keeps its confirming action.
- **Hover**: `button::drawn` finds every button in the last frame (a run of
  a rest fill with its caps) and `button::light_hovered` lifts the one under
  the pointer, at draw time, whatever layer is on top. `hover_moved` repaints
  when the pointer enters or leaves one.
- **Segmented control** (`button::segmented`): a field's box holding the
  choices, the chosen one a chip in `segment` (the desktop's canvas chip,
  as Contact Us draws its kinds); the track takes the focus tint; a segment
  under the pointer lifts. Used for General's Theme and the
  workflow editor's Builder | Form.
- **Dropdowns**: a field with `▾` at its right (pickers, the Keybindings
  context filter, the workspace form's Machine, the project switcher).
- **Title bars** (`button::bar`): the editors' back, title and badge,
  Builder | Form, Discard changes, Save changes and `⋮` on one three-row bar.
  Narrow, the least-needed pieces go first (Discard changes, then the
  segmented control; both are in the `⋮` menu or on a key), then the title
  is cut.
- **Section headings** (`settings::section_heading`): title over
  description with the section's buttons at the right, level with them, as
  the desktop's; under them when there's no room.
- **Dialogs** (`settings::overlay_panel`, `render_dialog`): the command
  palette's panel (muted single-line border, the `search` fill), the
  desktop's heading inside (title, then what it does), buttons at the right
  in the desktop's order. Fields in a dialog are wells (`input_well`) so
  they read on the panel. Menus use the same panel, the selected entry the
  table's half-block pill.

- **Overlays outside Settings** that share these pieces now use them:
  Contact Us's Send Message / Cancel / Close (full-width, the desktop's
  stacked layout, via `Button::lines_in`), About's Close and
  Acknowledgements' Done (Primary) and View source (Secondary), via
  `Button::draw` and `hovered` for views that track their own hover. The
  shell runs the same drawn-frame hover pass over any overlay, so Contact
  Us's buttons lift under the pointer too.

New theme tokens (shared `Theme`, both themes, contrast-tested):
`input_well`, `button`, `button_lit`, `button_primary`,
`button_primary_lit`, `button_primary_text`, `button_danger`,
`button_danger_lit`, `button_danger_text`, `button_focus`,
`button_danger_focus`, `button_focus_text`. Contact Us's `primary_focus`
(its own focused Send) is gone with its own buttons; `primary` and
`primary_label` stay for its megaphone badge.

## Findings (page, mismatch, fix)

Sidebar
- [x] Project section: `▾ proj` in cyan; desktop shows the project name with a chevron at the right → name bold, `▾` at the right.
- [x] Sections, order and numbers already matched (General 1, Keybindings 2, Terminal 3; Workspaces 5, Workflows 4, Agents 6, the desktop's deliberate order).

General
- [x] Theme was bracket-less text segments → segmented control with hover and focus.
- [x] "Clear the editor setting", "Edit config.yaml", the Open in editor list and About Shelbi were `› label  note` link rows → Secondary buttons (the list flows into bands; a lone one keeps its note beside it).
- [ ] Desktop General has no page title; the TUI keeps "General" and its line, as every TUI page opens with its heading (no window title in a terminal). The desktop-only controls (transparency, blur, CLI path, notifications) have no TUI counterpart.

Keybindings
- [x] Title row had no buttons; desktop puts Open keys.yaml beside the title → heading button.
- [x] Filters were three separate rows (search, `Context ‹ All contexts ›`, `Reset all  Open keys.yaml`) → one row as the desktop's: search, context dropdown (`▾`), Reset all (disabled when nothing is customized). Stops follow the drawn order.
- [x] Group titles were cyan → loud white, as the desktop's group headers.
- [x] Chords were cyan text → keycaps on the Secondary button fill; the picked one takes the button focus; None and `+` quiet.

Terminal
- [x] "Your terminal's colors" with Regular/Bright rows and `blk red …` abbreviations → the desktop's "ANSI colors" section: its description, a Color | Regular | Bold table with full color names (read-only here: the terminal's own).

Workspaces
- [x] Separate page title + TUI description, then `› New workspace…  n` → the Workspaces section heading is the page's (desktop wording: "Give agents their own place to work, on this Mac or another machine.") with New workspace at its right.
- [x] Machines heading was cyan with `Add machine…  m` → loud heading, desktop description "Choose where this project’s workspaces run.", Add machine at its right; a full-width rule between the sections.
- [x] Desktop footnotes added: "Each workspace has its own worktree…", "Remote machines connect over SSH…" (the TUI's Slot, +tag and Reachable notes stay: the desktop says them in tooltips).
- [x] Failed read: "Try again" link → the desktop's panel: "Couldn’t read this project’s workspaces", the error, a Retry button.

Workspace dialog
- [x] Box-drawn frame in the accent color, title in the border → palette panel, heading inside.
- [x] Name and Machine stacked → side by side, the Machine menu drops under its column.
- [x] Footer buttons as bracketed text → rule, then Remove workspace (Danger, left, edit only), Cancel and Create workspace / Save changes (Primary, right).
- [x] Remove confirmation → shared dialog: Cancel, Remove workspace (Danger), Cancel focused.

Machine dialog
- [x] Read-only "Kind" row the desktop doesn't have → removed (Add is SSH; the local form shows no SSH fields).
- [x] SSH help under the host field → the desktop's footnote under the Name | SSH host pair (plus the TUI's "An empty SSH host uses the name").
- [x] Test connection / Install CLI as text → buttons (Install CLI Primary first, as the desktop's); the desktop's "Shelbi checks the remote CLI…" footnote under the check.
- [x] Footer → rule, Cancel, Add machine / Save changes (Primary).

Workflows
- [x] Page title, then a second "Workflows" heading → one heading with New workflow at its right.
- [x] Statuses: `+ New status`, `Edit statuses.yaml in your editor` rows → Edit YAML and New status in the heading, as the desktop's; a rule between the sections.
- [x] Row menu labels: `Delete…` → Delete; `Edit statuses.yaml` → Edit YAML.
- [x] Keyboard order follows the drawn order (the heading's buttons before each table); a first read still lands on the first workflow.

Workflow editor
- [x] One-row bar with `← Back`, `task default`, text segments, `⋮ Actions` → three-row title bar: back, name with the Default badge, Builder | Form segmented, Discard changes, Save changes (Primary), `⋮`.
- [x] Name, Description, Initial status stacked → side by side (the desktop's identity row) when they fit.
- [x] Section titles cyan → loud; the Statuses list gained the desktop's column headers (Status, Owner, Agent, Workspace tags).
- [x] `+ Add status`, `Manage project statuses`, `+ Add transition`, `Delete workflow…` links → buttons (Delete workflow is Danger).
- [x] Builder panel: Set as initial, Add transition, Remove from workflow (Danger), Delete transition (Danger), Cancel connection, Cancel + Add transition (Primary) → buttons; panel title loud.
- [x] Delete and conflict confirmations, the leave prompt → shared dialog in the desktop's order (Cancel, then the action).

Agents
- [x] Page title + `+ New agent  n` → heading with New agent at its right.
- [x] Footnote wording → the desktop's ("…in Workflows…"), the Workflows link a button.
- [x] Shared instructions: `Edit instructions  in place` row → button in the heading; while typing, Cancel and Save (Primary) in the heading, as the desktop's.
- [x] Reset/Delete confirm strip → Cancel and Reset / Delete (Danger) buttons.
- [x] Row `⋮` menu → palette panel, the selected entry a pill.

Agent editor
- [x] Title bar as the workflow editor's (badge Default / Customized).
- [x] Name and Description side by side.
- [x] `+ Add skill`, the skill form's Add skill / Cancel, Reset to default, `Delete agent…` → buttons (Add skill Primary, Delete agent Danger).
- [x] "Instructions" title cyan → loud.

Contact Us, About, Acknowledgements (landed alongside, #1940 and #1938)
- [x] Contact Us drew its own buttons (Send on `primary`, focus `primary_focus`; Cancel on `dialog_control`, focus the field tint), no hover → the shared Primary / Secondary buttons at full width, focus and disabled (Sending…) alike, lifting under the pointer.
- [x] About's Close and Acknowledgements' Done / View source drew solid accent or selection fills → the shared buttons (Done Primary, Close and View source Secondary), keeping their own hover tracking through `hovered`.
- [x] Settings' segmented chip used `selection`; Contact Us's used `segment` → both `segment`.

Leave prompt (every page and editor)
- [x] "Unsaved changes" box with Save, Discard, Cancel → the desktop's "Save changes to {name}?", "Your edits to this workflow/agent/page haven’t been saved.", Cancel, Discard, Save (Primary) left to right; ←/→ follow the drawn order.

## Needs a model change (not done here)

- The instructions and SKILL.md previews wrap by characters (`Shelb` / `i's`): the editor's text model wraps by cells for caret mapping, shared with typing. Word wrap there is a `shelbi_app` text-model change.
- Contact Us keeps its `dialog` panel (half-block edge) and `dialog_control` fields where Settings' dialogs use the palette's panel with `input_well` fields: two dialog surfaces, one button set. Moving Contact Us onto `overlay_panel` would also change its desktop-matched centered layout, so it's left for a design call.
- The editors' heading buttons (Add status, Manage project statuses, Add transition, Add skill) sit under their lists, not in the section heading as the desktop's: the editors' keyboard order and Tab-by-section stepping follow the list, and moving them is a change to `items()` and the section stepping.
- The desktop hides Discard changes until there are edits; the TUI shows it dimmed so the keyboard order doesn't shift.

## Verified

- `cargo test -p shelbi-tui` (913 + 2), `cargo test -p shelbi-app`, `cargo clippy --workspace --all-targets -- -D warnings`; desktop `cargo clippy --all-targets -- -D warnings` and `cargo test` (781).
- New tests: the widget (looks per kind and state, rows, bands, hover lift, bar dropping, segmented), section headings, fields side by side, the dialog panel and narrow buttons, a button lifting under the pointer through the shell (Settings and Contact Us), Esc on the leave prompt keeping a typed command unsaved.
- Eye check in a sandboxed root (`SHELBI_ROOT` in a temp dir, private `tmux -L`), captured at 160x45, 100x30 and 60x20, dark and light, rendered to PNG.
