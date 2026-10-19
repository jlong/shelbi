# dt-menus-notifications: native menus, key equivalents, system notifications

## What landed

- **The menu bar** (`src/menus/`): Shelbi, File, Edit, View, Go, Window, Help.
  - `menus/tree.rs` is a pure function `build(CommandModel, Keymaps,
    WindowFacts) -> MenuTree` (no gpui). Every command
    `CommandRegistry::commands` offers for the window's current state lands in
    a menu, placed by its `CommandKind` family (`place`, an exhaustive match
    with no wildcard):
    - **Shelbi**: About, Settings… ⌘,, Services, Detach, Detach Other Clients,
      Quit Project…, Quit Shelbi and Stop All Agents…, Hide ⌘H, Hide Others ⌥⌘H,
      Show All, Quit Shelbi ⌘Q.
    - **File**: Add Project…, Switch Project… (the sidebar switcher), Open
      Project ▸ (one per other project, by label), Edit Configuration ▸ (the
      registry's edit targets), Close Window ⌘W.
    - **Edit**: Cut, Copy, Paste, Select All.
    - **View**: Command Palette, Hide/Show Sidebar ⌃⌘S, Focus Sidebar, Focus
      Main Area, Turn Zen Mode On/Off, Error Log.
    - **Go**: the nav views, Workspaces ▸, Reviews ▸ <task> ▸ (Open Review,
      Approve, Reject…), Agents ▸ (only when a legacy agent exists).
    - **Window**: Minimize ⌘M, Zoom; AppKit appends the project windows (see
      `../gpui.md`).
    - **Help**: Shelbi Documentation (shelbi.dev/docs), Report an Issue.
  - The registry's fixed commands keep their slot when the registry leaves
    them out and are drawn disabled (Detach Other Clients with no other client,
    Quit Project with no project). Families that come and go show a disabled
    placeholder when empty ("No Workspaces", "No Reviews", ...).
  - A menu item runs its registry command id through `CommandKind::from_id`
    and the window's `run`, exactly the palette's path (relaunch and bootstrap
    gates included); an open overlay closes first, as for a palette pick.
  - `menus/mod.rs` converts the tree to gpui menus and owns the actions:
    `MenuInvoke { target, generation }` for most items, the standing actions
    for the items with a fixed shortcut (`OpenSettings`, `CloseWindow`,
    `Quit`, `Hide`, the Edit actions, ...), and `MenuUnavailable` (handled by
    nothing) for disabled items. `install` compares with the installed tree,
    so the window offers a tree on every change and AppKit only rebuilds when
    something moved.
  - **Rebuilds**: the active window offers its tree on activation, every feed
    snapshot (Zen on/off, workspaces and reviews), keymap loads, palette-input
    refreshes (other projects, edit targets, other clients; re-read on
    activation and when the palette opens), and a sidebar toggle.
- **Key equivalents from `keys.yaml`**: the global `open_palette`,
  `focus_sidebar`, `focus_main` and `zen_toggle` chords show on their items
  (⌃P, ⌃H, ⌃L, ⌥Z by default) and move with a rebinding. A chord becomes a key
  equivalent only when a menu can carry it (Ctrl, Option or Cmd held, or a
  function key) and macOS doesn't own it. The macOS standards (⌘, ⌘W ⌘Q ⌘H
  ⌥⌘H ⌘M ⌃⌘S ⌘X ⌘C ⌘V ⌘A) are one table, `tree::FIXED_SHORTCUTS`, which
  `menus::init` binds and the tree displays.
- **In-app keys**: the same `keys.yaml` chords drive the window. New here: the
  Zen toggle chord (⌥Z) now toggles Zen from anywhere in the window, a
  focused chat or workspace terminal included (the window takes it in the
  capture phase like the focus and palette chords). Ctrl+C stays the terminal's.
- **Reserved chords**: `tree::reserved_conflicts` lists every `keys.yaml`
  binding (any mode) on a chord macOS owns (⌘W, ⌘Q, ⌘Space, ⌃Space, ⌃↑, ...).
  The settings view shows one red line per conflict ("global.open_palette is
  bound to ⌘W, which macOS uses for Close Window: it won't reach Shelbi."),
  and such a chord is never used as a key equivalent. The window re-reads
  `keys.yaml` whenever it comes to the front, so an edit shows on return.
- **Edit menu**: ⌘C / ⌘V / ⌘A are bound in the terminal's `Terminal` key
  context to the Edit actions, which the terminal handles: Copy (only offered,
  so only enabled, with a selection), Paste (bracketed when the program asked
  for it; into the find bar while it is open), Select All (scrollback plus
  screen; just the grid on the alternate screen). With an overlay open, Paste types into it and Copy copies the error
  log. With a gpui-kit text input focused (board title, settings fields) the
  window forwards Cut/Copy/Paste/Select All as the input's own actions. With
  none of those, the items are gray.
- **⌘, opens settings**, from the menu or the key.
- **Quit semantics** (the orchestrator's answer, see Decisions): ⌘Q closes
  this client, every window, with no confirmation; agents and the daemon keep
  running (the TUI's `q`). Quit Project… and Quit Shelbi and Stop All Agents…
  are their own Shelbi-menu items with the existing confirmation dialogs and
  **no** key equivalent.
- **System notifications** (`src/notify.rs`, pure; wiring in `app.rs`):
  - A window's `notify::Tracker` watches its feed. Transitions, never levels:
    a workspace badge entering `AwaitingInput` ("alpha is waiting for you") or
    `AwaitingPermission` ("alpha needs permission"), including input to
    permission; a task id newly in the review column ("Ready for review:
    <title>"); the orchestrator's `shelbi:<state>` title marker going from
    working to idle ("The orchestrator is waiting for you") or blocked ("...
    needs permission"). The first loaded snapshot only primes (opening a window
    never replays what was already waiting), a cold skeleton is ignored, a
    `Loading` badge only primes.
  - The orchestrator's title comes from the refresh pipeline: after each
    snapshot the feed thread probes the orchestrator session's title over a
    short-lived, never-attached session connection (`refresh::
    orchestrator_title`, the protocol's `info` request, 500ms handshake bound)
    and sends a change as the same `FeedEvent::Session` title notice the
    attached chat view sends.
  - `notify::decide`: off when its switch is off; suppressed while the
    window is frontmost and showing the item (that workspace, the Issues board
    for a review, the chat for the orchestrator); held back within 2 minutes
    of the same item's last notice (a flapping badge); otherwise posted.
    Leaving the waiting state dismisses the item's notification.
  - Each notice's tag is `shelbi/<project>/<ws/<name>|review/<id>|orch>`;
    reposting a tag replaces the notification. A click (registered once in
    `main.rs`) opens or focuses the project's window and runs
    `FocusWorkspace` (the Workspace view, its sidebar row selected), opens the review task's
    detail on the board, or shows the chat.
  - **Settings ▸ Notifications**: three switches, saved to
    `config.yaml::desktop.notifications` (`workspace_blocked`, `review_ready`,
    `orchestrator_waiting`; missing = on, only an off switch is written). A
    save reloads the window's switches at once; other windows reload on
    activation.

### Shared crates

- `shelbi_state::DesktopNotifications` (in `user_config.rs`, exported): the
  `desktop.notifications:` block, with `set` and on-by-default getters, and
  `DesktopConfig::is_empty` counting it. Serde-additive (old files load, an
  all-on block isn't written), so no config-upgrade sniffer: no shipped
  template changed.
- Nothing else moved: the menu tree and the notice rules are macOS-client
  concerns the TUI has no counterpart for.

## Decisions

- **⌘Q is the client quit, not Quit Shelbi.** The task said "Cmd+Q →
  QuitShelbi semantics as the TUI: confirm, then quit the project/all". I
  asked; the orchestrator answered: keep ⌘Q quitting the desktop client with
  no confirmation (the macOS convention, and the TUI's `q` / Ctrl+C close the
  TUI without stopping anything); put Quit Shelbi and Quit Project in the
  Shelbi menu with the TUI's confirmation and no default key equivalent, so a
  stray chord can never stop the agents. The app's item is titled "Quit
  Shelbi"; the registry's is "Quit Shelbi and Stop All Agents…" so the two
  read differently.
- **Disabled = no handler.** AppKit autoenables menu items through gpui's
  `is_action_available`, so `MenuItem::disabled` alone doesn't gray an item;
  disabled items get an action nothing handles. See `../gpui.md`.
- **`keys.yaml` shortcuts are menu labels, the keys stay the window's.** The
  key equivalent comes from a binding in a key context no element declares,
  so gpui never fires it from the keyboard; the window's existing `keys.yaml`
  handling keeps the keys (and AppKit only consults the menu when the window
  didn't handle the key, so nothing fires twice). Rewiring the in-app keys
  onto gpui bindings would have changed the chat terminal's capture-phase
  ordering for no gain. Stale label bindings can't be removed in gpui, so each
  new shortcut set gets a new generation (rare: only a `keys.yaml` change).
- **The orchestrator "needs an answer" = it stopped for the user**, the same
  signal the task counts for workspaces (awaiting input or a dialog), read
  from the session's title marker. Workers' clarification questions reach the
  user through the orchestrator's chat, so they surface as this notice too.
- **Reviews notify on reaching the column**, not on the slot serving.
- **A review notice is "on screen" while the Issues board shows**: the
  desktop has no review view yet (`dt-review-view`), and the board is where
  the card sits; clicking opens its detail there.
- **The Window menu's project list is AppKit's**, not ours: gpui makes the
  menu named "Window" the app's Windows menu, and AppKit lists each project
  window by title with the key one checked.
- **No `objc2` bridge**: `gpui-pre 0.3.9` already has system notifications.

## Not finished / known gaps

- **Notifications post only from the bundled `Shelbi.app`.** Under `cargo
  run` gpui's center is off (no bundle identifier), so the post is a logged
  no-op. I verified the decisions live (below) but did not post a real banner
  or click one: that needs the signed bundle and the macOS permission prompt
  on the user's screen. Worth a by-hand check from `scripts/bundle.sh`'s app.
- About is a native alert (version, CLI version, docs URL), not the standard
  About panel (gpui has no API for it).
- Undo/Redo aren't in the Edit menu (nothing in the app supports them).

## Verification

Rebase note: rebased onto `jlong/desktop-app` at `bd3ed502`
(`dt-terminal-complete`, #1705); kept both `DesktopConfig` additions
(`option_as_meta` and `notifications`), all Settings sections, and routed the
terminal's ⌘V / Edit ▸ Paste through one find-bar-aware `paste_clipboard`.
After it: desktop `cargo build` ok, `cargo clippy --all-targets -- -D
warnings` ok, `cargo test` 264 passed; root `cargo check --workspace
--locked` ok, `cargo clippy --workspace --all-targets -- -D warnings` ok,
`cargo test -p shelbi-state` 882 passed.

From `desktop/`:

- `cargo build`: ok.
- `cargo clippy --all-targets -- -D warnings`: ok.
- `cargo test`: 232 passed after rebasing onto `dt-workspace-view` (#1704)
  (206 before). New: `menus::tree` (bar order, every available
  registry command in an enabled item, unavailable fixed commands disabled,
  the Shelbi menu's order and ⌘Q vs the agent-stopping items, Go's views /
  workspaces / per-review submenu, placeholders, project labels, `keys.yaml`
  chords and rebindings as key equivalents, Zen/sidebar titles, menu
  eligibility and rendering of chords, the fixed shortcuts, no conflicts in
  the defaults, a reserved rebinding reported and kept off the menu, placement
  by family), `menus` (every menu keystroke parses as a gpui keystroke and
  converts back to the same chord; which items dispatch `MenuInvoke`),
  `notify` (priming, loading skeletons, workspace waiting / permission /
  clearing, quiet idle/paused/loading, review arrival and clearing, the
  orchestrator marker, on-screen suppression per target, switches, cooldown,
  tag round trip), `refresh` (the orchestrator probe runs after refreshes and
  sends only changes), `settings` (switches round-trip through the config),
  `app` (⌥Z is taken from a focused terminal).

From the repo root (I touched `crates/shelbi-state`):

- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: ok.
- `cargo check --workspace --locked`: ok (root `Cargo.lock` unchanged).
- `cargo test --workspace` (with this pane's `TASK_ID` / `PROJECT` /
  `SHELBI_*` unset): 3970 passed, 0 failed, across 49 test binaries (before
  the rebase). After rebasing onto #1704: clippy and `check --locked` again
  ok, `cargo test -p shelbi-state` 881 passed.

By hand, in an isolated `SHELBI_HOME` (its own daemon; project `web` with two
workspaces; the orchestrator a script flipping its title between
`shelbi:working` and `shelbi:idle` every 12s):

- An AX dump of the running app's menu bar matched the tree: every menu and
  item, ⌃P / ⌃H / ⌃L / ⌥Z / ⌘, / ⌘W / ⌘Q / ⌘H / ⌥⌘H / ⌘M / ⌃⌘S on their items,
  Detach Other Clients and the empty-family placeholders gray, Settings and
  Close Window gray until bootstrap finished, and AppKit's "✓web" in Window.
- Edit, with the chat terminal focused and the app frontmost: Paste and
  Select All enabled, Copy gray until there was a selection, Cut gray. ⌘V
  (posted to the app) and Edit ▸ Paste both pasted into the orchestrator
  session (checked with `shelbi session snapshot`); Edit ▸ Select All then
  Edit ▸ Copy put the whole scrollback on the clipboard. (Validation runs
  against the key window: an item pressed while the app wasn't key stayed
  inert, as AppKit intends.)
- ⌥Z posted to the app with the chat terminal focused opened the Zen intro
  once (no double fire from the menu's ⌥Z key equivalent).
- Pressing Shelbi ▸ Settings… and Go ▸ Issues through AX ran them (with the
  app frontmost; with another app frontmost there is no active window to take
  a window action, as for a user).
- Notification decisions in the debug log: `OnScreen` while the window was
  frontmost on the chat, `Post` once another app was frontmost.
- A `keys.yaml` binding `open_palette` to ⌘W showed the red conflict line in
  settings.
