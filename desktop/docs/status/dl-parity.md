# dl-parity: the parity walk on Omarchy

The last check before `desktop-linux` merges: the Mac app's parity walk
(`docs/parity.md`), on a packaged install on Omarchy, with what failed fixed
here or filed.

## How it was walked

- **The packages.** `shelbi-bin` from a release build of the CLI, archived
  in the release's layout, and `shelbi-desktop-bin` from
  `desktop/scripts/linux-archive.sh`, each rendered by
  `scripts/release/render-aur-package.sh --archive` and built with
  `makepkg`. Their contents match the PKGBUILDs (the binaries, the desktop
  entry, the hicolor icons 16 to 512, the plugins, the licenses), and
  `shelbi-desktop --print-version` finds the CLI. The walk ran the binaries
  from the unpacked packages: installing needs root, which a worker pane
  doesn't have.
- **The sandbox.** Its own Shelbi root, daemon and sessions, with the hub's
  socket variables unset. Projects `pa` and `pb`, each a git repo with a
  local bare `origin`, a `dev` workspace and a `review-1` slot (tag
  `review`). A fake `claude` that prints a transcript and waits. `editor:
  nvim`. On `pa`: a backlog issue, a todo issue depending on it, and an issue
  in review on a pushed branch.
- **The desktop.** Hyprland 0.56 on Omarchy, the app on its own workspace.
  Keys went through `wtype`, the mouse through a small
  `zwlr_virtual_pointer_v1` client, and screenshots came from `grim`. A
  nested, headless Hyprland was tried first so the walk wouldn't take the
  user's screen, but its headless output never redrew.
- **The rows.** Every command row was walked, then the platform differences,
  then each view-model field as it showed. The Omarchy columns in
  `parity.md` record each row. A second pass on the fixed build checked
  every fix and the states the first pass missed (New task's menus, the
  board's filters and search, Machines with work assigned, the workspace
  panel with a task).

## What it found and fixed

| Found | Fix |
|---|---|
| Close Project, Close All Other Projects and Quit Shelbi confirmed in gpui's fallback prompt: a white box with unstyled text, its detail clipped | `views/prompt.rs`, the app's own dialog (the overlays' scrim and panel, the Button component; Return takes the focused button, Esc Cancel, Tab and ←/→ move), installed with `App::set_prompt_builder` off macOS |
| "Projects on this Mac" and "Open a project on this Mac" on the launch screen, "on this Mac" in the Add Project wizard, the Workspaces page and Open in Terminal View's line | "this computer" off macOS (`shelbi_app::launch`, `add_project_wizard`, `LOCAL_PHRASE`) |
| Settings ▸ General: the App icon help named the Dock and Finder, and the Notifications help said "macOS asks for permission" | Linux lines: the icon theme, the notification daemon |
| Settings ▸ Terminal: Omarchy's JetBrainsMono Nerd Font read "Not installed", and the picker listed only the bundled faces (the installed list was CoreText's) | `fonts::installed` reads fontconfig (`fc-list`) off macOS: spacing 90+ or "Mono" in the name, never an emoji font |
| Settings ▸ Terminal showed the Option key setting ("as in other Mac apps"), which does nothing on Linux, and the scrollbar help named System Settings | The Option key setting is macOS-only; the scrollbar help names the desktop's overlay scroll bars setting |
| The Changelog footer said `⌥↓/⌥↑` and `⌘F`; the New task footer "⌘ Return to create"; the task pane's resize tooltip `⌥↑ ⌥↓`; the diff draft `⌃J` | The platform's labels: Alt+Down/Alt+Up, Ctrl+Shift+F, "Ctrl+Enter to create", Alt+↑ Alt+↓, Ctrl+J |
| The worktree's Open tooltip said Finder | "Open the worktree in the file manager" |
| The tab bar kept an empty gap for macOS's traffic lights | `sidebar::TRAFFIC_LIGHTS_RIGHT` is 0 off macOS |
| Move to Window listed two windows on one project as two identical rows | `tab_bar::distinct_labels`: `pa`, `pa (2)` |
| The palette listed two "Quit Shelbi" rows (the registry's, which stops everything, and the menu's client quit) | The client's is "Quit (agents keep running)" in the Linux palette |

## What it filed

On the project's board (its tracker is the file system):

- `desktop-a-project-still-in-orchestrator-onboarding-loses-its-tab-and-its-daemon`:
  a project opened but not yet launched isn't in the daemon's open set, so
  the daemon idle-exits with nothing else open, and the tab goes when the
  window switches away. Shared code.
- `desktop-a-new-tab-opened-from-the-palette-s-switch-project-takes-no-keys`.
  Shared code.
- `desktop-board-start-right-after-assign-is-refused-as-changed-elsewhere`:
  Start sent the revision from before the Assign landed. Shared code.
- `desktop-views-break-in-a-narrow-window-a-tiled-half-screen`: Hyprland
  tiles a second window at half width, where the orchestrator onboarding is
  clipped and some Settings tables collapse.
- `desktop-the-visual-check-feature-doesn-t-build-on-linux`:
  `VisualTestAppContext` is macOS-only in gpui-pre.

## Decisions

- **Walked from the unpacked packages, not a system install.** The worker
  pane has no root. The packages' contents and the binary's own discovery
  were checked; launching from Walker after `pacman -U` is left to a human
  with the built packages (or the AUR once a release is out).
- **CLI discovery stays login-shell first.** The packaged app found
  `~/bin/shelbi` ahead of `/usr/bin/shelbi`, as designed (a user's own build
  wins). The README now says so and how to pin `desktop.cli_path`.
- **Confirmations are the app's dialog on Linux only.** macOS keeps its
  native sheet; the builder is installed under `cfg(not(target_os = "macos"))`.
- **Hide the Option key setting rather than relabel it.** Linux's Alt is
  always Meta, so there's nothing to choose. The saved value is kept, so a
  config shared with a Mac is untouched.
- **Shared-code bugs are filed, not fixed here.** They need the daemon's
  open set, the board's revision tracking or responsive layouts, each its
  own change.

## Not covered

- Remote machines, GitHub and Linear trackers, errors in the error log, a
  serving review URL: the sandbox had none. The fields that only show then
  are marked "not exercised" in `parity.md`.
- The Omarchy theme switch, notifications, the clipboard and drops are their
  subtasks' own checks (`dl-omarchy-theme`, `dl-platform`, `dl-input`).
- Board drag and drop: a synthetic `zwlr_virtual_pointer_v1` drag never
  started a card drag, but a real mouse did. Tab tear-off worked either way.
- Seen once and not confirmed: a relaunch with `shelbi-desktop pa` opened
  `pb`, although `window_state::launch_plan` gives a named project the first
  window.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings` and `cargo test`
  in `desktop/`.
- `cargo clippy -p shelbi-app --all-targets --locked -- -D warnings` and
  `cargo test -p shelbi-app --test parity_doc --test command_scenarios` at
  the root; `Cargo.lock` and `desktop/Cargo.lock` are unchanged.
- New tests: `prompt::tests` (keys, button styles),
  `fonts::tests::fc_list_lines_make_families_and_the_monospaced_ones`,
  `changelog::tests::the_footer_names_this_platforms_chords`,
  `tab_bar::tests::move_to_window_numbers_a_repeated_title`, and the
  traffic-light constants per OS.
- By hand: the walk above, then each fix on the fixed build.
