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
work. `dt-spike-terminal` evaluated both (below) and **the terminal element is
built on `gpui-pre` + `gpui-kit` today**. The orchestrator makes the final
call; a switch records its reason in this file.

### The channel evaluation (`dt-spike-terminal`, 2026-10-14)

Both channels were built from the same sources (`desktop/` with the terminal
element) against **the same gpui code**: the git channel pins
`zed-industries/zed@93d745668805029476e6d7fbb19652116fed51de`, the commit
`gpui-pre 0.3.9` was cut from. So the difference is the channel, not gpui.

```toml
# The git channel, as built:
gpui = { git = "https://github.com/zed-industries/zed", rev = "93d745668805029476e6d7fbb19652116fed51de" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "93d745668805029476e6d7fbb19652116fed51de", features = ["font-kit"] }
```

**Does it build?** Both do, with Rust 1.99.0 and no change to the terminal
element. What the switch needed:

- **`gpui_platform`'s `font-kit` feature, explicitly.** `gpui-pre` turns it on
  by default; Zed's crate doesn't (Zed's own workspace enables it). Without it
  the app builds and runs but logs `gpui_macos was compiled without the
  font-kit feature, so no text will be rendered` and draws no text at all.
- **Dropping `gpui-kit`**, which pins `gpui-pre` and can't sit beside a git
  gpui. Six lines change: `gpui_kit::init` and its theme call in `main.rs`, and
  the gpui-kit asset fallback in `assets.rs`.
- The visual check's `gpui_platform/test-support` feature exists in both.

**Numbers.** Apple M4, 10 cores, Rust 1.99.0. Each is a clean build into a
fresh target dir, run back to back on a shared hub whose load average was
50–90 from other workers' builds, so treat wall times as rough and compare
within a row. The one-time Zed git fetch took 5m32s and keeps a 397 MB git db
plus a 492 MB checkout in `~/.cargo/git`.

| | `gpui-pre` + `gpui-kit` | Zed git (no widget library) |
|---|---|---|
| Debug, clean | 618 s (load ≈57) | 455 s (load ≈62–78) |
| Debug, warm (touch `element.rs`) | 18 s | 8 s |
| Debug binary | 92 MB | 46 MB |
| Release, clean (`lto = "thin"`, `codegen-units = 1`) | 1307 s (load ≈53–61) | 977 s (load ≈54–71) |
| Release binary (stripped) | 13.7 MB | 7.8 MB |
| `target/` after both | 4.0 GB | 2.5 GB |

The size and time gap is almost all `gpui-kit` (gpui-component, gpui-base,
their tree-sitter grammars, markdown, charts), not the channel: the gpui code
is identical. A git-channel app that grows its own widgets would close much
of that gap.

**What `gpui-kit` gives us** (longbridge's gpui-component, Apache-2.0):

- *Lists*: `List`/`ListState` over a `ListDelegate`, virtualized
  (`v_virtual_list`), with sections, loading state, and a `searchable_list`.
  That covers the palette, the issues list, activity, and machines.
- *Inputs*: single-line `Input` (search, number, OTP, clear button), `textarea`,
  and a code `editor` with tree-sitter highlighting. That covers the palette
  query, task edit forms, and settings.
- *Menus*: context, dropdown, and popup menus, an app menu bar, and native
  menus.
- *Markdown and rich text*: `gpui-base`'s text layer (the `markdown` crate),
  with frontmatter and code highlighting. That covers task bodies and the
  review panel.
- Also: dock and resizable panels, dialogs, sheets, notifications, tabs,
  tables, tooltips, a sidebar, and a status bar.

**What `gpui-pre` couldn't do for the element: nothing.** Every gpui API the
element uses (custom `Element`, `shape_line` with `force_width`, `paint_quad`,
`window.on_mouse_event`, `Window::handle_input` + `EntityInputHandler`,
`FocusHandle`, `VisualTestAppContext` + `render_to_image`) is the same in
both, because it is the same code.

**Recommendation: stay on `gpui-pre` + `gpui-kit`.** The element gives no
reason to switch, and the widget library is worth the extra build time for
the views the remaining subtasks build. The git channel stays a one-commit
fallback (`Cargo.toml` + the six `gpui-kit` lines) if the snapshot publisher
goes quiet. Watch out for `font-kit` if it's ever taken.

### How the app uses gpui-kit today

- `gpui_kit::init(cx)` installs the component layer (its theme plus inputs,
  lists, dialogs, notifications), ready for the subtasks that adopt its
  widgets. `src/theme.rs` keeps gpui-kit's light/dark mode in step with
  Shelbi's resolved theme (`gpui_kit::component::Theme::change`).
- `src/assets.rs` falls back to gpui-kit's assets so its components' internal
  glyphs resolve. Shelbi's own icons are Heroicons, not gpui-kit's Lucide set.
- The Issues board (`dt-issues-board`) uses three gpui-kit widgets: the
  Markdown `TextView` (task bodies; links open in the browser by default),
  `Textarea` (the in-place body editor) and `Input` (the new-issue title).
  All three work without gpui-kit's `Root`; the window is still not wrapped
  in it. The first subtask that uses a dialog or notification opens the
  window through `gpui_kit::open_window` (or wraps it in `Root`).
- The `markdown` crate is a direct dependency pinned to the version
  gpui-base resolves (`=1.0.0`), so the task detail's checklist count and
  its tests read the same parse the `TextView` renders. Bump it with
  gpui-kit.

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
| 2026-10-14 | `gpui-pre 0.3.9` | (no upgrade) Zed git @`93d7456`, evaluated in a scratch copy | No text rendered: `gpui_macos` without `font-kit`. `gpui-kit` can't coexist with a git gpui. | `features = ["font-kit"]` on `gpui_platform`; drop `gpui-kit` (6 lines). Not switched. |

### Tests

`[dev-dependencies]` repeats `gpui-pre` with its `test-support` feature, for
`#[gpui::test]` and `VisualTestContext` (view tests open a headless test window
and find elements by `debug_selector`). Same version, so the pin is unchanged.

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
- **Window appearance.** `Window::appearance()` and
  `Context::observe_window_appearance` report and follow the system's
  light/dark switch live (checked 2026-10-14 by flipping macOS dark mode under
  `cargo run`). `App::set_window_appearance(Some(..))` pins the window chrome
  for the `theme: dark` / `light` overrides; while pinned, the window stops
  tracking the system, so `auto` passes `None`.
- **Flex items size to their content.** A `flex_1` child of a flex row
  defaults to `min-width: auto`, so a wide child (the board's columns) widens
  the whole main area instead of scrolling inside it, and absolutely
  positioned overlays then center on the oversized box. Give the container
  `min_w_0()` (and `overflow_hidden()`); the main area and the board both do.
- `cargo` reports a future-incompatibility warning for `block 0.1.6`, pulled
  in by gpui's macOS platform crates. Upstream's to fix; harmless today.
- **Font metrics report the descent signed** (negative, below the baseline):
  `text_system().descent(..)` is negative for Geist Mono, so a line height is
  `ascent + descent.abs()`. Shaped `LineLayout`s report it positive.
- **`Window::resize` on an offscreen `VisualTestAppContext` window does nothing
  synchronously**: it dispatches `setContentSize` to the real main queue, which
  never drains without a run loop. The visual check resizes a wrapper view
  instead.
- `VisualTestAppContext::capture_screenshot` renders the *last* drawn scene;
  call `window.draw(cx).clear(cx)` first to render a fresh frame.

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

**2026-10-14, with the terminal element (`dt-spike-terminal`).** Measured on
a shared hub at load average 50–90, so not comparable to the quiet-machine
row above. Clean `gpui-pre` + `gpui-kit`: debug 618 s / 92 MB, release
1307 s / 13.7 MB, warm debug rebuild after touching `element.rs` 18 s. The
Zed-git comparison is in "The channel evaluation" above.
