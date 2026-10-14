# The gpui channel

gpui has no stable release channel. The crates.io `gpui` release (0.2.2,
October 2025) is a year old, and Zed's main branch has split gpui into
platform crates it doesn't publish. This file records what the desktop app
pins, why, and every breakage an upgrade causes. Every gpui version choice
lives in `desktop/Cargo.toml` and here, so switching channels is one commit.

## What is pinned

| Crate | Version | Renamed to | Role |
|---|---|---|---|
| `gpui-pre` | `=0.3.9` | `gpui` | gpui itself, a weekly snapshot of Zed's gpui (this one is `zed@93d7456`, published 2026-10-12) |
| `gpui-pre-platform` | `=0.3.9` | `gpui_platform` | `application()`: picks the macOS (or Linux) platform |
| `gpui-kit` | `=0.7.3` | | the widget layer (gpui-base, gpui-component, gpui-kit-assets): dock, lists, inputs, markdown, dialogs |
| Rust | `1.99.0` | | `rust-toolchain.toml`; recent snapshots track recent stable |

The two `gpui-pre` crates are renamed back to their upstream names in
`Cargo.toml`, so the source says `gpui::` and `gpui_platform::` exactly as it
would against a pinned Zed git dependency. A channel switch is then a
`Cargo.toml` edit, not a source rewrite.

`gpui-kit` pins `gpui-pre` to one exact snapshot (0.7.3 requires
`gpui-pre =0.3.9`), so the two always move together: pick the `gpui-kit`
release you want, then set `gpui-pre` to the snapshot it requires
(`cargo tree -i gpui-pre` shows it).

## Why this channel

The plan weighs two live options:

| Channel | For | Against |
|---|---|---|
| `gpui-pre` snapshots + `gpui-kit` | Fast start; dock, lists, inputs, markdown ready-made; on crates.io | Weekly churn; depends on one outside maintainer |
| Zed git dependency at a pinned revision | Authoritative; upgrade on our own schedule | No widget library; heavy checkout; can't publish |

The foundation starts on `gpui-pre` + `gpui-kit`, the plan's default if both
work. **`dt-spike-terminal` makes the real decision**: it evaluates the git
dependency and `gpui-kit`'s inputs, lists, menus, and markdown, and may
switch. A switch records the reason in this file.

### How the app uses gpui-kit today

- `gpui_kit::init(cx)` installs the component layer (its theme, set to dark,
  plus inputs, lists, dialogs, notifications), ready for the subtasks that
  adopt its widgets.
- `src/assets.rs` falls back to gpui-kit's assets so its components' internal
  glyphs resolve. Shelbi's own icons are Heroicons, not gpui-kit's Lucide set.
- No view uses a gpui-kit widget yet, and the window is not wrapped in
  gpui-kit's `Root`. The first subtask that uses a dialog or notification
  opens the window through `gpui_kit::open_window` (or wraps it in `Root`).

## Upgrading

1. Pick the new `gpui-kit` release and read its changelog.
2. Set `gpui-kit`, then `gpui-pre` and `gpui-pre-platform` to the snapshot
   it requires. Bump `rust-toolchain.toml` if the snapshot needs it.
3. From `desktop/`: `cargo update`, then build, clippy, test, and
   `cargo run` to look at the window.
4. Add a row to the breakage log below, even when nothing broke.

## Breakage log

| Date | From | To | What broke | Fix |
|---|---|---|---|---|
| 2026-10-14 | | `gpui-pre 0.3.9`, `gpui-kit 0.7.3` | Initial pin. | |

### Known quirks

- **Unbundled builds and the bundle identifier.** `cargo run` runs a bare
  binary, which macOS names in the menu bar after the executable
  (`shelbi-desktop`). `build.rs` embeds `macos/Info.plist` with
  `CFBundleName = Shelbi` to fix that. That plist must **not** carry a
  `CFBundleIdentifier`: with one, gpui's notification setup (run by
  `gpui_kit::init`) believes it is in a real bundle, calls
  `UNUserNotificationCenter`, and the app aborts with
  `bundleProxyForCurrentProcess is nil`. `dt-packaging`'s real `.app` has
  both; `dt-menus-notifications` should expect system notifications to work
  only from the bundle.
- **`NSProcessInfo.setProcessName` does not rename the menu bar app** for an
  unbundled binary; the embedded plist is the fix that works.
- `cargo` reports a future-incompatibility warning for `block 0.1.6`, pulled
  in by gpui's macOS platform crates. Upstream's to fix; harmless today.

## Build time and binary size

The foundation window (sidebar/main split, no views). Clean builds of the
whole dependency graph on the machine below; `debug = false` for
dependencies, release is `lto = "thin"`, `codegen-units = 1`, symbols
stripped.

Measured 2026-10-14 on an Apple M4 (10 cores), macOS 26.6.2, Rust 1.99.0,
`gpui-pre 0.3.9` / `gpui-kit 0.7.3`, after `cargo clean`:

| Profile | Clean build (wall) | CPU (user + sys) | Binary size |
|---|---|---|---|
| debug (`cargo build`) | 3m 14s | 408s + 45s | 85 MB (88,974,784 bytes) |
| release (`cargo build --release`) | 6m 17s | 887s + 54s | 12 MB (13,046,144 bytes) |

An incremental debug rebuild after touching `src/main.rs` takes about 4s.
`desktop/target` is about 3.9 GB after both clean builds.

Re-measure after a gpui upgrade or a channel switch, and add a row here
rather than replacing these.
