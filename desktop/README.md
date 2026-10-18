# Shelbi desktop

The native macOS client for Shelbi, built with [gpui](https://www.gpui.rs/).
It is a sibling of the TUI (`crates/shelbi-tui`) at feature parity: both
render over `shelbi-app` (navigation, the command registry, view models,
background refresh) and talk to the same daemon and sessions. The plan is
"Desktop App (gpui)" (`Plans/desktop-app-gpui.md` in the Shelbi ContextStore).

Status: feature parity with the TUI. Every command in the shared registry
runs in both clients (`docs/parity.md` records the walk), every view renders
natively, and no not-implemented path is left. On launch the app finds the
`shelbi` CLI, starts the daemon, and checks versions; a window shows one
project at a time and switches projects in place (File ▸ New Window, ⌘⇧N,
opens another), with the sidebar, the Orchestrator chat, the Issues board and task
detail, Activity, Machines, the Workspace view (an agent with a user shell
beside it, local or remote), and the Review view; the command palette and the
overlays (error log, add project, load for review, reject, Zen intro); native
menus and system notifications. `scripts/bundle.sh` packages it as a signed,
notarized `Shelbi.app`, shipped as the `shelbi-desktop` Homebrew cask. Each
piece's story is in its status doc (`docs/README.md` lists them).

The app is a client of the `shelbi` CLI and needs it installed
(`brew install jlong/shelbi/shelbi`); the cask depends on the formula.

## Settings

The app reads `~/.shelbi/config.yaml` (shared with the CLI and the TUI). The
settings view (⌘,) edits the `desktop:` block, and terminal settings apply to
open terminals at once.

```yaml
theme: auto                          # auto (follows the system), dark, light; shared with the TUI
editor: code --wait                  # edit:* commands, "Open in editor", Cmd+click; then $EDITOR, then vim
desktop:
  cli_path: ~/src/shelbi/target/debug/shelbi   # skip discovery (login PATH, Homebrew, ~/.cargo/bin)
  terminal_font_family: Geist Mono   # the default; bundled with the app
  terminal_font_size: 13             # points, 6 to 72
  option_as_meta: false              # true, left, or right (Ghostty's shape)
  notifications:                     # every kind is on unless switched off
    workspace_blocked: true          # an agent waits for input or a permission
    review_ready: true               # a task reaches the review column
    orchestrator_waiting: true       # the orchestrator waits for an answer
```

| Key | Default | What it does |
|---|---|---|
| `theme` | `auto` | Color theme. `auto` follows the system appearance live (the TUI reads `auto` as dark). |
| `editor` | `$EDITOR`, then `vim` | The editor for `edit:*` commands, a task's "Open in editor", and Cmd+click on a path. A terminal editor opens in Terminal.app; a GUI editor directly. |
| `desktop.cli_path` | discovered | The `shelbi` the app runs. A leading `~/` is expanded. |
| `desktop.terminal_font_family` | Geist Mono | Terminal font. |
| `desktop.terminal_font_size` | 13 | Terminal font size in points (6 to 72). |
| `desktop.option_as_meta` | `false` | Whether Option is Meta in terminals: `true`, `left`, or `right`. |
| `desktop.notifications.*` | all on | Which system notifications are posted (only from the bundled app; an unbundled `cargo run` has no bundle id). |
| `keys.yaml` | built in | Key bindings, shared with the TUI; the menus show them as key equivalents. |

Window state (frame, sidebar width, sidebar collapsed) is saved per window
slot in `~/.shelbi/desktop-windows.json`, with the project each window last
showed and whether it was open: a launch reopens the windows that were open.
Where each project was left (view, sidebar selection and scroll, open review,
board filter) is in `~/.shelbi/projects/<project>/desktop-view.json`, so a
switch back lands there. (The old per-project `desktop-window.json` is read
once, by the first launch after the change.) Environment:
`SHELBI_PROJECT` (or a project argument) picks the first window's project, `RUST_LOG` the log level (default
`warn`), `SHELBI_DESKTOP_UPDATE_CHECK=0` turns the update check off, and
`SHELBI_DESKTOP_UPDATE_URL` points it at a fixture (see Update notice).

## Build and run

macOS on Apple silicon, macOS 15 or later.

```sh
cd desktop
cargo run              # the most recently launched project
cargo run -- <project> # or a named one ($SHELBI_PROJECT works too)
```

`rust-toolchain.toml` pins the toolchain, so rustup installs it on first use.
Set `RUST_LOG` (default `warn`) to see more of the log on stderr.

## Its own workspace

`desktop/` is a **separate Cargo workspace**: it has its own `Cargo.lock` and
`rust-toolchain.toml`, and the root `Cargo.toml` lists it under `exclude`.
No root `--workspace` command sees this crate, and gpui's dependency tree can
never move the root lockfile or the CLI's Rust 1.88 floor. It depends on the
shared crates by path (`../crates/...`).

The package version is a literal (a version can't be inherited across
workspaces). Keep it equal to the root `[workspace.package] version`;
`scripts/release/check-version.sh` fails the release when they differ.

## Rules

- **gpui types stay in this crate.** `shelbi-app` and `shelbi-state` must
  never depend on gpui; `crates/shelbi-app/tests/no_ui_deps.rs` enforces it.
  If a shared crate needs something for the desktop, add a toolkit-free API
  there and adapt it here.
- **Every gpui version choice lives in `Cargo.toml` and `docs/gpui.md`**, so
  switching gpui channels is one commit.
- **Exhaustive command dispatch.** `src/commands.rs` matches every
  `shelbi_app::CommandKind` and `View` variant with no wildcard arm, so a new
  upstream variant fails to compile here until the desktop handles it (the
  TUI's effect match is wildcard-free too). Never add a `_ =>` arm, and never
  bring back a not-implemented path: a command lands in both clients at once.
- **Icons are [Heroicons](https://heroicons.com/)** (MIT, v2.2.0), vendored
  under `assets/icons/heroicons/` and served by `src/assets.rs`. Every icon
  is the outline drawing (`24/outline`) rendered at 20×20, the mini size:
  draw one with `assets::heroicon(icons::X)`; no view calls `svg()` or sizes
  an icon itself (a test guards it). Add an icon by copying its SVG from
  `optimized/24/outline/` at that tag, then adding a row to the `heroicons!`
  list in `src/assets.rs`. Don't use gpui-kit's bundled icon catalog in
  Shelbi's own UI.
- **Colors come from `src/theme.rs`**, the shared tokens of
  `shelbi_app::theme` (one named set, dark and light, also used by the TUI)
  as gpui colors. Views read them with `cx.global::<Theme>()`. No color
  literals in views; a missing color is a new token in `shelbi_app::theme`.

## Parity with the TUI

Both clients run the same commands over the same models; the plan's
"Keeping parity honest" is four checks, all part of `cargo test`:

| Check | Where | Runs in |
|---|---|---|
| Exhaustive matches, no wildcard arm | `src/commands.rs` (`dispatch_command`, `sends_to_daemon`, `dispatch_view`), `src/menus/tree.rs` (`place`); the TUI's `dispatch_effect` in `crates/shelbi-tui/src/shell/mod.rs` | the compiler |
| Headless scenario tests, one per command: the `Effect` and the model change | `crates/shelbi-app/tests/command_scenarios.rs` | root `cargo test` |
| Smoke test per front end: every registry command through its real UI path | desktop: `src/app_smoke.rs` (palette Enter and the menu item, gpui test context); TUI: `every_registry_command_runs_from_the_palette` in `crates/shelbi-tui/src/shell/mod.rs` (`TestBackend`) | each workspace's `cargo test` |
| View-model field checklist | `docs/parity.md`, guarded by `crates/shelbi-app/tests/parity_doc.rs` | root `cargo test` |

Adding a command means: a `CommandKind` variant and its effect in
`shelbi-app`, a scenario in `command_scenarios.rs` (the file won't compile
without one), and an arm in both clients (neither will either). Adding a
view-model field means a row in `docs/parity.md` naming where each client
shows it. Platform differences are allowed where the platform differs (drag
and drop, native menus, system notifications are desktop-only); the commands
under them are shared. `docs/parity.md` lists the ones that exist.

## Architecture map

```
desktop/
  Cargo.toml           the workspace + the shelbi-desktop package; gpui pins
  Cargo.lock           this workspace's own lockfile (committed)
  rust-toolchain.toml  the exact Rust version
  build.rs             embeds macos/Info.plist so `cargo run` is "Shelbi" in the menu bar
  macos/Info.plist     the unbundled binary's name (no CFBundleIdentifier: see the file)
  macos/Shelbi.app.plist  the .app bundle's Info.plist (bundle.sh fills in the version)
  scripts/bundle.sh    build, bundle, sign, notarize, staple, zip, sha256 (see Packaging)
  scripts/render-icon.sh  assets/icon/shelbi.svg -> shelbi-1024.png
  assets/icon/         the app icon: SVG source + the 1024px PNG master bundle.sh cuts into .icns
  assets/icons/        vendored Heroicons + their LICENSE
  assets/fonts/        Geist (UI) and Geist Mono (terminal), OFL; Symbols Nerd Font Mono (terminal icons); with licenses
  docs/gpui.md         the gpui channel: what is pinned, why, breakage log, build numbers
  docs/README.md       the status index: every subtask's status doc
  docs/parity.md       the TUI parity record: view-model fields, the command walk
  docs/status/         one <task-id>.md per subtask
  src/main.rs          gpui App, the app-wide globals, the launch window
  src/menus/           the menu bar: tree.rs builds it as data from the CommandModel + keymaps
                       (pure, tested); mod.rs converts it to gpui menus and owns their actions
  src/notify.rs        system notifications: transitions to notices, when to post or suppress (pure)
  src/app.rs           a window's root view: sidebar | main area, routing through
                       ClientState; the open windows and their slots (Shell); switching
                       a window's project in place
  src/app_smoke.rs     the smoke test: every registry command via the palette and the menus
  src/app_window_tests.rs  the window model: switch in place, A→B→A, New Window, notices, quit, launch
  src/keys.rs          gpui keystrokes to shelbi_state::keymap KeyChords (so keys.yaml applies)
  src/refresh.rs       the refresh pipeline: a project's feed (refresher, change
                       subscription, polling fallback) and the control-socket watcher
  src/feeds.rs         one feed per project, shared by the windows that show it
  src/projects.rs      the project catalog (+ skeleton prefetch) and opening a project
  src/window_state.rs  window slots (frame, sidebar), the launch plan, per-project view state
  src/commands.rs      exhaustive dispatch over CommandKind and View
  src/theme.rs         shelbi_app::theme tokens as gpui colors; theme: setting, live system appearance
  src/assets.rs        the asset source (Heroicons, then gpui-kit's assets)
  src/platform/        bootstrap: login env, CLI discovery, daemon start, version check, relaunch;
                       --print-version (print_version.rs), the daily release check (update.rs)
  src/views/           bootstrap screens, sidebar, chat, issues, activity, machines, workspace, review, palette
  src/elements/        custom elements, chiefly the terminal (elements/terminal/)
  src/drops/           image and file-promise drops onto a terminal (AppKit hooks on gpui's window)
  src/spike.rs         `--spike <session>`: one terminal element on one session
  src/visual_check.rs  `--visual-check <dir>` (feature `visual-check`): offscreen render + input checks
  src/bench/           `--bench-term` (feature `bench`): the terminal performance bench and its helpers
  src/bench_push.rs    `--bench-push` (feature `visual-check`): frame timing of the column push
```

Which subtask fills what:

| Module | Subtask |
|---|---|
| `elements/` terminal element, gpui channel decision | `dt-spike-terminal`, `dt-terminal-complete`, then `dt-terminal-perf-multiclient` |
| `theme.rs` light theme, shared token names | `dt-theme-tokens` |
| `platform/` | `dt-platform-bootstrap` |
| `views/` sidebar, project switching, refresh pipeline, settings | `dt-shell-sidebar` |
| `views/` chat | `dt-chat-view` |
| `views/` issues board and task detail | `dt-issues-board`, `dt-board-dnd` |
| `views/` activity and machines | `dt-activity-machines` |
| `views/` palette and overlays | `dt-palette-overlays` |
| `views/` workspace | `dt-workspace-view` |
| `views/` review | `dt-review-view` |
| native menus, notifications | `dt-menus-notifications` |
| bundle, signing, cask, update notice (`platform/update.rs`, `views/update.rs`) | `dt-packaging` |
| smoke tests (`app_smoke.rs`), `docs/parity.md`, docs | `dt-parity-audit` |

## The terminal element

`src/elements/terminal/` is every session the app shows (the chat, a
workspace's agent, a review's editor and diff). What it does, with the
TUI's rules where the TUI has the feature (the shared code is
`shelbi-term`):

| | |
|---|---|
| Scroll | The wheel, Shift+wheel (always Shelbi's), Shift+PageUp/PageDown/Home/End, and the scrollbar. New output doesn't move a scrolled-up view; "N lines below" jumps back. On the alternate screen (`less`, `man`) the wheel sends arrow keys. |
| Select | Drag (cells), double-click (a word), triple-click (a line), Alt+drag (a block). Shift keeps a gesture Shelbi's while a program reports the mouse. Dragging past the top or bottom edge scrolls. A finished selection is copied; Cmd+C copies too. Works on both screens; it follows its text as output scrolls and goes away when that text changes. |
| Paste | Cmd+V, bracketed when the program asked. |
| Find | Cmd+F: search as you type (smart case), Enter / Shift+Enter (or Cmd+G / Cmd+Shift+G) for older / newer matches, Escape to close. |
| Links | Hold Cmd to underline a URL or a `path[:line[:col]]` under the pointer; Cmd+click opens a URL in the browser and a file in your editor at that line (the `editor` setting, then `$EDITOR`, then `vim`; terminal editors open in Terminal.app). Relative paths resolve against the session's working directory. |
| Keys | The TUI's encoder (kitty protocol, DECCKM). Option types characters and dead keys unless `option_as_meta` says otherwise; Option with Enter, arrows, Backspace, or Delete is always Meta. IME composition draws at the cursor. |
| Mouse | Clicks, drags (`?1002`), and motion (`?1003`) reach a program that asks for them, SGR or X10 encoded. Alt hands a drag to a reporting program. |
| Fonts | `terminal_font_family` / `terminal_font_size`, live. Geist Mono and Symbols Nerd Font Mono are bundled; a glyph the font lacks falls back to Geist Mono, then the Nerd symbols, then Menlo, then the system (CJK, emoji). |

### Accessibility

Scope for this release: **every part of the app is keyboard-operable** (the
sidebar, the palette, every overlay, the board's cards and dialogs, the
Review and Workspace panels, the menus with their key equivalents), and **the
terminal is readable by VoiceOver**. The native views other than the
terminal (sidebar rows, board cards, panels) don't publish AccessKit nodes
of their own yet, so VoiceOver reads the menu bar and the terminal but not
those; that is the next accessibility step.

The terminal publishes its screen through gpui's AccessKit integration while
an assistive technology (VoiceOver) is running:

- **Supported:** the pane is a focusable `Terminal` node named after the
  session; its children are the visible rows as text runs (wide characters
  once, trailing blanks trimmed), and the cursor is the node's text
  selection, so VoiceOver reads the screen, navigates it by line, word, and
  character, and reports the cursor position. The tree follows scrolling
  and output on the next frame.
- **Not supported yet:** scrollback beyond the visible rows (scroll to bring
  history on screen first), announcing new output as it arrives (no live
  region, which would make a busy agent unusable to listen to), text
  attributes (color, bold), and the selection or search matches as
  accessible state. The find bar, the scrollbar, and the "lines below"
  button are mouse/keyboard controls without accessible actions of their
  own. AccessKit costs nothing while no assistive technology is connected.

### By hand

```sh
cd desktop
shelbi session new --name spike/sh -- zsh -l     # or `-- claude`
cargo run -- --spike spike/sh                    # one window, one terminal element
RUST_LOG=shelbi_desktop=debug cargo run -- --spike spike/sh   # + frame stats every 2s while painting
cargo run --features visual-check -- --visual-check /tmp/vc   # offscreen: frames + input checks, no screen needed
cargo run --features visual-check -- --visual-workspace /tmp/vw   # offscreen: the Workspace view's states, fake sessions
```

### Pixel QA against the Figma mockups

The redesign's target is John's Figma file; its 1x frame exports (and the
frame list, tokens and decisions) live in
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/`. Two tools compare the
app against them:

```sh
cd desktop
cargo run --features visual-check -- --visual-figma /tmp/vf   # 20 frames: 10 screens x 2 themes
scripts/figma-diff.sh /tmp/vf                                 # diff each against its reference
```

`--visual-figma <dir>` renders every mockup screen through the real window
shell (`ShelbiWindow`) at 1440x1024, offscreen, with no screen, daemon or user
state: a sandboxed `SHELBI_HOME` under `/tmp` (hub and control socket variables
unset), fake sessions and a fake daemon. The fixtures carry the mockups' own
content: "My project", the sidebar's reviews and workspaces, the board's
cards and counts, the Activity events (the clock pinned to 2026-10-17 15:00
local, so relative times and day groups read as drawn), the Machines rows
(the `gpu` SSH box answers reachable without a probe), and the terminal
transcripts with their SGR colors (redrawn at the pane's size, so footers sit
on the last rows). Frames are named as the references are,
`<screen>-<theme>.png` for orchestrator, issues, issue-detail, activity,
review, workspace, workspace-idle, machines, settings and command-palette;
`SHELBI_VISUAL_THEME=dark|light` renders one theme. The hidden window draws
at the main display's backing scale; a 2x frame is downsampled, so the saved
frames are always 1x and line up with the references pixel for pixel. For
close inspection pass `--2x` (`--visual-figma /tmp/vf2 --2x`): each window
then opens on the first Retina display (visible there while it renders, above
other windows, without taking focus) and the native 2880x2048 frame is kept.
With no Retina display attached (a closed laptop on external monitors) it says
so and the frames come at 1x. Export the Figma frames at 2x (Figma's export
panel, or the Figma MCP's `get_screenshot` on the node IDs in the README
there) into a directory to pass as the diff's ref-dir.

`scripts/figma-diff.sh <render-dir> [ref-dir]` (ImageMagick 7; ref-dir
defaults to the folder above) writes `<screen>-<theme>-side.png` (reference
left, render right), `-heat.png` (the per-pixel difference, red to yellow,
over the dimmed reference) and `-overlay.png` (50/50 blend) into
`<render-dir>/diff/`, and prints a table, also saved as `diff/report.txt`: the
share of pixels differing by more than `FUZZ`% (default 10) and the bounding
boxes of the largest regions of differing pixels (merged across `GROW` px,
default 6). The references' rounded window corners sit on Figma's canvas and
are masked out (`CORNER`, default 28 px). A render of another size is scaled to
the reference's first. A region as large as the frame means the background
itself differs.

### Performance and concurrent clients

`dt-terminal-perf-multiclient` (numbers in `docs/status/dt-terminal-perf-multiclient.md`):

```sh
scripts/bench-term.sh                  # the bench: a real window, sandboxed sessions, ~2 min
scripts/bench-term.sh --input yes --source direct --skip-many   # one case
scripts/multiclient-check.sh           # app + TUI + shelbi attach on one session, typing in turn
scripts/idle-cpu.sh                    # the full app's idle CPU with one attached session
```

The column push (`dt-instant-panel-push`, numbers in
`docs/status/dt-instant-panel-push.md`): input to the first moving frame, and
each frame's time through the push, opening and leaving a review and a
workspace over fake daemons:

```sh
cargo run --release --features visual-check -- --bench-push              # a real window, the display's frames
cargo run --release --features visual-check -- --bench-push --offscreen  # no display needed (a locked screen)
```

All three run in a sandboxed Shelbi root under `/tmp` with the hub's socket
variables unset. `multiclient-check.sh` and `idle-cpu.sh` drive the
`shelbi` CLI in `$SHELBI_BIN` (default `PATH`), which must speak this
branch's control protocol for the full app (`target/debug/shelbi` from the
repository root does). The bench and the concurrent-client check open
windows without taking focus; `idle-cpu.sh` runs the real app, which does.

Never point `--spike` at a real workspace's session: the window reports
its size on attach (which the session follows while no other client is
active) and typing makes it the active client, so the agent's PTY would be
resized to the spike window.

## Packaging

`scripts/bundle.sh` builds the distributable app: a release build for
`aarch64-apple-darwin`, assembled into `target/bundle/Shelbi.app` (bundle id
`dev.shelbi.desktop`, the version from `Cargo.toml`, the icon from
`assets/icon/`), signed, notarized, stapled, then zipped as
`target/bundle/Shelbi-<version>-arm64.zip` with a `.sha256` beside it. It
bundles no CLI: the app is a client of the `shelbi` formula, and the Homebrew
cask (`shelbi-desktop`) depends on it.

```sh
desktop/scripts/bundle.sh              # build + bundle (+ sign/notarize when it can)
desktop/scripts/bundle.sh --no-build   # re-bundle the last release build
open desktop/target/bundle/Shelbi.app
desktop/target/bundle/Shelbi.app/Contents/MacOS/Shelbi --print-version
```

Each step runs only when its credentials are present, and the script ends by
saying which build it produced (`SIGNED + NOTARIZED + STAPLED`, `SIGNED, NOT
NOTARIZED`, or `UNSIGNED`):

- **Signing** uses `SIGN_IDENTITY`, by default `Developer ID Application:
  32pixels, LLC (6L5M94R7K7)`, when that identity is in your keychain. It signs
  inside-out (any nested code, then the bundle; never `--deep`) with
  `--options runtime --timestamp`. Without the identity the app is ad-hoc
  signed: it runs on your Mac, nowhere else.
- **Notarization** runs `notarytool submit --wait`, then `stapler staple` and
  `spctl --assess`, when the app is Developer ID signed and you provide either
  `NOTARY_PROFILE` (a `xcrun notarytool store-credentials` keychain profile) or
  an App Store Connect API key: `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`, and the
  `.p8` as a path in `NOTARY_KEY_PATH` (or base64 in `NOTARY_KEY_P8`, which is
  what CI passes). For example:

  ```sh
  NOTARY_KEY_PATH=~/private_keys/AuthKey_<id>.p8 NOTARY_KEY_ID=<id> \
    NOTARY_ISSUER_ID="$(cat ~/.appstoreconnect/issuer_id.txt)" \
    desktop/scripts/bundle.sh
  ```

  Credentials that are set but wrong fail the script rather than quietly
  producing an un-notarized build.

`assets/icon/shelbi.svg` is the icon's source (the brand's pixel "S" on the
macOS icon grid). After editing it, run `scripts/render-icon.sh` (needs
ImageMagick) and commit the regenerated `shelbi-1024.png` too; `bundle.sh` only
needs macOS's `sips` and `iconutil`.

The release workflow (`.github/workflows/release.yml`, job `desktop-release`)
runs the same script on every tag, with the certificate imported from
secrets into a temporary keychain, and publishes the notarized zip to the
GitHub release and the `shelbi-desktop` cask in `jlong/homebrew-shelbi`. The
secret names and the flow are in `docs/release.md` and
`docs/release/homebrew-tap.md` at the repository root.

### Update notice

Once a day, off the UI thread, the app asks the GitHub releases API for the
latest Shelbi release (`src/platform/update.rs`). When it is newer than the
app, every project window shows a bar with the command that upgrades it,
`brew upgrade --cask shelbi-desktop`; otherwise it shows nothing. There is no
in-app updater. The last answer is cached in `~/.shelbi/desktop-update-check.json`,
so a relaunch inside the day shows the same notice without asking again.

To see the notice without waiting for a release, point the check at a
fixture (this bypasses the cache in both directions):

```sh
echo '{"tag_name":"v99.0.0","draft":false,"prerelease":false}' > /tmp/release.json
SHELBI_DESKTOP_UPDATE_URL=file:///tmp/release.json cargo run
```

`SHELBI_DESKTOP_UPDATE_CHECK=0` turns the check off.

## Verifying a change

Every subtask runs these before handing off:

```sh
# Always, from desktop/:
cd desktop
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
```

When the change touches a shared crate under `crates/`, also run the root
workspace checks from the repository root, and confirm the root `Cargo.lock`
is unchanged:

```sh
cargo check --workspace --locked
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +1.88 check --workspace --all-targets --locked   # the CLI's MSRV
```

The desktop `cargo test` includes the smoke test (`app::smoke`); the root
`cargo test --workspace` includes the scenario tests, the TUI smoke test, and
the `docs/parity.md` guard. A failing parity check is a real gap: fix the
client (or the table), never the check.

CI: `.github/workflows/desktop-ci.yml` runs the three desktop commands on a
`macos-14` (arm64) runner. It is manual only (`workflow_dispatch`, input
`ref`); run it from the Actions tab against your branch.

## Status docs

`docs/README.md` indexes every subtask's status doc. Each subtask writes
`docs/status/<task-id>.md` and keeps it current as it lands. Include:

- **What landed:** the user-visible behavior, and which `commands.rs` arms
  it made real.
- **What's left:** known gaps, deferred items, follow-ups.
- **Decisions:** calls made along the way and why.
- **Verification:** the commands run and their results, plus anything checked
  by hand in the running app.

The umbrella's own record is `docs/status/desktop-app.md`.
