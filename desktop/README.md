# Shelbi desktop

The native macOS client for Shelbi, built with [gpui](https://www.gpui.rs/).
It is a sibling of the TUI (`crates/shelbi-tui`) at feature parity: both
render over `shelbi-app` (navigation, the command registry, view models,
background refresh) and talk to the same daemon and sessions. The plan is
"Desktop App (gpui)" (`Plans/desktop-app-gpui.md` in the Shelbi ContextStore).

Status: the app shell. On launch the app finds the `shelbi` CLI, starts the
daemon, and checks versions (`src/platform/`, see
`docs/status/dt-platform-bootstrap.md`); then each project gets its own window
with the real sidebar (live workspace and review state), project switching,
the refresh pipeline and a settings view (`docs/status/dt-shell-sidebar.md`),
in the theme the `theme:` setting picks (`auto` follows the system
appearance). The Issues nav row shows the native board and task detail
(`dt-issues-board`, see `docs/status/dt-issues-board.md`); the other views
show a "Not implemented yet" stub until their `dt-*` subtask lands.
`scripts/bundle.sh` packages it as a signed, notarized `Shelbi.app`, shipped
as the `shelbi-desktop` Homebrew cask (`docs/status/dt-packaging.md`).

The app is a client of the `shelbi` CLI and needs it installed
(`brew install jlong/shelbi/shelbi`). To point it at a particular build, set
`desktop.cli_path` in `~/.shelbi/config.yaml`:

```yaml
desktop:
  cli_path: ~/src/shelbi/target/debug/shelbi
```

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
  upstream variant fails to compile here until the desktop handles it. Never
  add a `_ =>` arm.
- **Icons are [Heroicons](https://heroicons.com/)** (MIT, 24px outline,
  v2.2.0), vendored under `assets/icons/heroicons/` and served by
  `src/assets.rs`. Add an icon by copying its SVG from the heroicons repo's
  `optimized/24/outline/` at that tag, then adding a constant and a table row
  in `src/assets.rs`. Don't use gpui-kit's bundled icon catalog in Shelbi's
  own UI.
- **Colors come from `src/theme.rs`**, the shared tokens of
  `shelbi_app::theme` (one named set, dark and light, also used by the TUI)
  as gpui colors. Views read them with `cx.global::<Theme>()`. No color
  literals in views; a missing color is a new token in `shelbi_app::theme`.

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
  assets/fonts/geist/  Geist, the UI typeface (OFL) + its LICENSE
  docs/gpui.md         the gpui channel: what is pinned, why, breakage log, build numbers
  docs/status/         one <task-id>.md per subtask
  src/main.rs          gpui App, the menu, the app-wide globals, the launch window
  src/app.rs           a project window's root view: sidebar | main area, routing
                       through ClientState, one window per project (Shell)
  src/keys.rs          gpui keystrokes to shelbi_state::keymap KeyChords (so keys.yaml applies)
  src/refresh.rs       the refresh pipeline: per-window feed (refresher, change
                       subscription, polling fallback) and the control-socket watcher
  src/projects.rs      the project catalog (+ skeleton prefetch) and opening a project
  src/window_state.rs  per-project window frame and sidebar width/collapse
  src/commands.rs      exhaustive dispatch over CommandKind and View
  src/theme.rs         shelbi_app::theme tokens as gpui colors; theme: setting, live system appearance
  src/assets.rs        the asset source (Heroicons, then gpui-kit's assets)
  src/platform/        bootstrap: login env, CLI discovery, daemon start, version check, relaunch;
                       --print-version (print_version.rs), the daily release check (update.rs)
  src/views/           bootstrap screens, sidebar, chat, issues, activity, machines, workspace, review, palette
  src/elements/        custom elements, chiefly the terminal (elements/terminal/)
  src/spike.rs         `--spike <session>`: one terminal element on one session
  src/visual_check.rs  `--visual-check <dir>` (feature `visual-check`): offscreen render + input checks
```

Which subtask fills what:

| Module | Subtask |
|---|---|
| `elements/` terminal element, gpui channel decision | `dt-spike-terminal`, then `dt-terminal-complete`, `dt-terminal-perf-multiclient` |
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
| smoke tests, docs | `dt-parity-audit` |

## The terminal element by hand

```sh
cd desktop
shelbi session new --name spike/sh -- zsh -l     # or `-- claude`
cargo run -- --spike spike/sh                    # one window, one terminal element
RUST_LOG=shelbi_desktop=debug cargo run -- --spike spike/sh   # + frame stats every 2s while painting
cargo run --features visual-check -- --visual-check /tmp/vc   # offscreen: frames + input checks, no screen needed
```

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

CI: `.github/workflows/desktop-ci.yml` runs the three desktop commands on a
`macos-14` (arm64) runner. It is manual only (`workflow_dispatch`, input
`ref`); run it from the Actions tab against your branch.

## Status docs

Each subtask writes `docs/status/<task-id>.md` (for example
`docs/status/dt-chat-view.md`) and keeps it current as it lands. Include:

- **What landed:** the user-visible behavior, and which `commands.rs` arms
  now do real work instead of the banner.
- **What's left:** known gaps, deferred items, follow-ups.
- **Decisions:** calls made along the way and why.
- **Verification:** the commands run and their results, plus anything checked
  by hand in the running app.

The umbrella's own record is `docs/status/desktop-app.md`.
