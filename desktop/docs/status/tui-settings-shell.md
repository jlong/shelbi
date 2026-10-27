# tui-settings-shell: Settings in the TUI, over a shared model

## The change

The TUI's only way to change configuration was to open a file in an
external editor. It now has Settings, laid out as the desktop app's
(`dt-settings-sections`): Settings stands in the sidebar's column with a back
button, the Global pages, and a project section headed by the project
switcher, with the shown page in the main area. This task is the foundation:
the shell, the navigation, and the hooks the follow-up pages (General,
Keybindings, Terminal, Workflows and Statuses, Agents, Workspaces and
Machines) build on. Its one page is a General summary.

Both clients now open Settings from the same registry command,
`action:settings` ("Settings").

## Shared (`shelbi_app::settings`)

Moved out of `desktop/src/views/settings_nav.rs` and the two desktop editor
states, toolkit-free:

- **`SettingsPage`, `SECTIONS`, `pages()`**: every page, its label, its fixed
  number (General 1, Keybindings 2, Terminal 3, Workspaces 5, Workflows 4,
  Agents 6, as before; Workspaces, from `dt-workspace-settings-index`, lists
  before Workflows) and its scope (`Scope::Global` or `Scope::Project`).
- **`PageSet`**: the pages a client offers. `PageSet::sections()` drops a
  titled section with nothing offered and always keeps the project section
  (its switcher picks the project Settings shows, even before a project page
  exists). The desktop offers `PageSet::ALL`; the TUI offers General.
- **`SettingsNav`**: the desktop's struct, now over a `PageSet`. It refuses a
  page its client doesn't offer, and `open(None)` reopens the page last shown.
  `SettingsNav::default()` offers every page, so the desktop's behavior and
  shortcuts are unchanged.
- **`LeaveGuard<T>`**: the unsaved-edits prompt's bookkeeping (leave, cancel,
  discard, save, go once the save lands, stay on a failed save). The desktop's
  workflow and agent editors used to each carry their own `prompt` /
  `after_save` pair; both now hold a `LeaveGuard`, with their public methods
  and tests unchanged.
- **`Job<T>` and `Ticket`**: a page's background load or save, keyed by the
  project it was for and a generation. `complete` takes a result only if its
  ticket is still the latest, so a load that lands after a project switch (or
  a newer request) is dropped; `reset` supersedes anything in flight.
- **`summary`**: the General summary's reads (`config.yaml` and its theme,
  `keys.yaml` validated read-only with its problem count, the resolved
  editor, and for the shown project: where its settings live, its workflows,
  statuses, agents and machines, and its Open in editor targets).

The registry gains `CommandKind::OpenSettings` / `Effect::OpenSettings`
(`action:settings`, listed on a blank query with the ⚙️ glyph, offered with or
without a project). The keymap gains `sidebar.open_settings`, bound to `,` by
default (built in, so existing installs get it with no `keys.yaml` change).

## The TUI (`crates/shelbi-tui/src/shell/settings.rs`)

**Entry.** The palette's Settings, the sidebar's `,` (the user's
`sidebar.open_settings`), or a click on the ⚙ at the end of the sidebar's
title row.

**Layout.** In the sidebar's column: the panels' square back button with
"Settings" beside it, the Global header over the page rows (the nav block's
half-block selection, each row with its number key), a rule, then
`▾ <project>`, the switcher. In the main area: the page, then the shell's
status line (when there is one) over a key-hint band. Nothing under Settings
is drawn, but nothing under it stops either.

**Settings is a layer over the view, not a view.** `main_view` keeps naming
what is underneath, so a background change (a review that closes, an async
open that lands) happens under Settings and Back shows its result. Sessions
stay attached and keep streaming; an open review or workspace interface stays
built. A navigation the user picks from the palette over Settings (a view, a
workspace, a review) closes Settings for it.

**Keys.** Nothing reaches a session while Settings shows (proven against a
real PTY in `pty_input_tests.rs`). Over the focused list, the sidebar
keymap's up / down / activate (`k` / `j` / Enter and Space by default) plus
the arrows; Tab and Shift-Tab (and the global Ctrl+H / Ctrl+L) move focus
between the sidebar and the page; a page's number opens it from either; on
the page, PageUp / PageDown / Home / End scroll and `r` reads it again. Esc
closes the innermost thing first: the palette or switcher over Settings, then
the unsaved-edits prompt, then Settings. `q` and Ctrl+C do nothing here (no
accidental quit). The hint band names the user's own up / down letters.

**Mouse.** A click opens a sidebar row, runs a page action, or answers the
prompt; the wheel moves the sidebar's selection or scrolls the page. Every
mouse action has a key.

**Back** (Esc, the back button, Enter on it) restores the focus and the
sidebar selection Settings was opened over. Settings reopens on the page last
shown, its selection on that page's row.

**Project switching.** The switcher is the palette's Projects column (the
TUI's switcher everywhere else too). A switch over Settings, from there or the
palette, first asks the shown page (it may hold unsaved edits); Settings then
stays up on the same page, now for the new project: its project data reloads,
and a load still in flight for the old project is dropped when it lands. Back
then lands on the new project's view with its sidebar focused, as any switch
does.

**The General summary.** This computer: the Settings folder, `config.yaml`
(its theme, "not created yet" when missing, or why it doesn't read),
`keys.yaml` (built-in defaults or your overrides, with a problem count), the
editor. The project: where its settings live (in the repository or on this
computer), the folder, workflows and statuses, agents, machines. Then its
Open in editor rows, the palette's `edit:*` targets: Enter runs the same
editor handoff, and the summary reloads when the editor returns. No project
open: "No project is open." with Switch project… and Add project…. A project
that doesn't read: the reason, with Try again, Edit Project Settings and
Switch project…. A failed read: the reason, with Try again (and `r`). The
page's machine count is the project's configured machines; the runtime
Machines view stays where it was (the palette).

**Narrow terminals.** On a page under 36 columns wide a field's value drops under its label;
the page scrolls, keeps the focused action in view, and says "↓ more" when
something is below. Tested at 80x24, 160x48, 64x14 (every action reachable by
keyboard) and down to 1x1 without a panic.

**Hooks for the follow-ups.** A page reports dirty and hands back its save
(`SettingsShell::begin_save`); the shell runs it off the UI thread and the
prompt (Save, Discard, Cancel; `s` / `d` / Esc, arrows and Enter) leaves once
it lands, or stays with the error on the page. General edits nothing, so in
this task only a test-injected edit makes it dirty. A page's loads go through
`begin_load` / `loaded` with a `Ticket` on one channel, so stale results are
always seen and dropped. A new page joins `settings::OFFERED` once it draws.

## The desktop

- `CommandKind::OpenSettings` reaches the window's `open_settings(None)`: the
  existing Settings, on the page last used. The app menu's Settings… is now
  that registry command's slot (still ⌘,; the ⌘, binding itself is unchanged),
  so the menu walk covers it.
- The sidebar's `sidebar.open_settings` (`,`) opens Settings there too.
- `views/settings_nav.rs` re-exports the shared types and keeps only what is
  gpui's: `PageLook` (each page's icon and row selector) and the render.
- No `shelbi_app::view` model changed, so `parity.md`'s field tables are
  unchanged; its command walk and platform differences gained Settings.

## Platform differences (by design)

- **Pages.** The desktop offers all five; the TUI offers General, a summary
  the desktop doesn't have (its General edits theme, CLI and notifications).
- **Shortcuts.** The desktop: ⌘, anywhere, ⌘1 to ⌘6 inside Settings. The
  TUI: no Command chords; `,` from the sidebar (rebindable), the palette, and
  the page's digit inside Settings.
- **The switcher.** The desktop's is the sidebar's menu; the TUI's is the
  palette's Projects column. A desktop switch reopens Settings on the new
  project's window state; the TUI's keeps Settings up in place.
- **Back's focus.** The desktop returns focus to the app sidebar on the
  view's row; the TUI restores the focus and selection Settings was opened
  over.

## Not done

- No page has editable settings yet, so the dirty prompt and the save path
  are exercised by tests only. The follow-ups wire them to real edits.
- A palette navigation over a dirty page doesn't ask first (only Back, a page
  change and a project switch do). No page can be dirty yet.
- No per-page palette entries ("Settings: Terminal").
- Not hand-walked in a live TUI against a sandboxed root; the shell tests
  drive the real event path (`handle_event`, `draw_frame` on `TestBackend`).

## Tests

- `shelbi-app`: `settings::tests` (numbering, page sets and sections, the
  keyboard walk over offered pages, reopening, `LeaveGuard`, `Job` stale
  drops); `tests/command_scenarios.rs`
  `settings_opens_over_the_view_on_the_last_page`; the registry and palette
  order tests.
- `shelbi-tui`: `shell::settings::tests` (stops, keys, a rebound `nav_down`,
  page actions per load state, the prompt, every load state's copy, tiny
  sizes) and the shell tests: entry from the palette, `,` and the gear; Back
  and Esc restoring view, selection and focus; reopening and Ctrl+H / Ctrl+L;
  nested Esc and typing into the palette over Settings; a project switch;
  a stale load dropped; 80x24 and 160x48; scrolling and resize; the Open in
  editor effects; mouse; the prompt; light readability. The palette smoke
  test walks `action:settings` with the rest; `pty_input_tests` proves no
  Settings key or paste reaches a real PTY and typing resumes after Back.
- Desktop: `commands.rs` dispatch tests, `app_smoke.rs` (palette and menu
  walks, now 23 commands), the menu tree tests, `settings_nav::tests`, and
  the editor states' leave tests over the shared guard.

## Verified

Root: `cargo build --workspace`, `cargo clippy --workspace --all-targets --
-D warnings`, `cargo +1.88 check --workspace --all-targets --locked`,
`cargo test --workspace`. Desktop: `cargo build`, `cargo clippy --all-targets
-- -D warnings`, `cargo test` (780 passed after rebasing on main). `Cargo.lock` unchanged.
