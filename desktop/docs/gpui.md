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
  both (its own plist, `macos/Shelbi.app.plist`, with
  `CFBundleIdentifier = dev.shelbi.desktop`): launched from the bundle with
  `open`, `gpui_kit::init` runs without the abort (checked 2026-10-14, macOS
  26, Developer ID signed with the hardened runtime and no entitlements).
  `dt-menus-notifications` should expect system notifications to work only
  from the bundle (`scripts/bundle.sh`).
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
- **Key dispatch runs capture (root to focused) then bubble (focused to
  root).** A focused terminal encodes and `stop_propagation`s every control
  key in its bubble handler, so a window-level chord the terminal must never
  see (Ctrl+H, Ctrl+Space) has to be taken in the window's
  `capture_key_down`. `dt-chat-view` does that only while the chat's terminal
  holds focus, so the board's text inputs keep their own Ctrl chords.
- **Focus listeners (`on_focus_in` / `on_focus_out`) fire when the window next
  draws**, not when `window.focus(..)` is called; a `TestAppContext` that only
  runs `run_until_parked` never sees them. And `Window::focused` keeps
  reporting a dropped entity's focus handle until that draw. To know whether a
  view held focus at some event, read `handle.is_focused(window)` at the event
  (`cx.subscribe_in` hands the subscriber the window), and compare
  `window.focused(cx)` against the old handle rather than `None`.
- Drag and drop (`dt-board-dnd`, gpui-pre 0.3.9): `on_drag(value, ctor)`
  starts after a 2px move and draws the constructor's view at the pointer
  minus the grab offset, so a ghost the card's width lines up with the card.
  `drag_over::<T>` styles apply only while `can_drop` passes, but a drop on an
  element whose `can_drop` is false is *consumed* (the active drag is taken and
  no listener runs), so a refusal can't explain itself there; the board keeps
  `can_drop` unset and refuses in its own `on_drop`. Drop listeners fire child
  first, and a child's `can_drop: false` would also swallow the drop before its
  parent sees it, so only attach `on_drop` to children that should take it.
  The drag constructor gets `&mut App`, not the view: reach the view through a
  `WeakEntity` (`cx.weak_entity()`). A drag released off every target never
  reaches a drop handler; poll `cx.has_active_drag()` to clear drag state.
- **Background-executor timers don't fire on an offscreen
  `VisualTestAppContext`** (no run loop): a view whose data arrives through a
  `cx.spawn` loop on `background_executor().timer(..)` never polls. The
  `--visual-views` check calls the views' `poll` (feature `visual-check`)
  between frames instead.
- **`gpui::list` re-anchors to the top on a full splice.** `ListState::splice`
  over a range containing the scroll anchor moves the anchor to the range
  start, and `reset` drops it. A feed that grows at the top every refresh
  (Activity) therefore uses a plain `overflow_y_scroll` container, whose
  pixel offset survives re-renders.
- **File drops** (`dt-workspace-view`). `on_drop::<ExternalPaths>` on an
  element receives Finder drops; a test drives one with
  `simulate_event(FileDropEvent::Entered { .. })` then `Submit`. gpui-pre
  0.3.9's macOS platform delivers only file drags (`ExternalPaths`), so
  dragging plain text from another app onto a terminal does nothing.
- **Measuring an element for a drag** (the Workspace view's split divider): a
  `canvas(prepaint, paint)` child with `absolute().size_full()` reports its
  bounds in prepaint; store them in the view and `cx.notify()` only when they
  change. `on_mouse_up_out` catches a release outside the element.
- **`VisualTestAppContext` has no `debug_bounds`** (only the test contexts
  do: the rendered frame is crate-private), so the offscreen harnesses check
  view state rather than drawn selectors.
- **`text_system().add_fonts` fonts can't be fallbacks.** gpui keeps added
  fonts in a font-kit memory source, which resolves a *primary* family, but a
  `Font`'s `fallbacks` become a CoreText cascade list that CoreText resolves by
  name among the fonts *it* knows. An embedded font named there is skipped and
  the glyph falls to LastResort (a box). `assets::load_fonts` also registers
  the terminal's fallbacks with `CTFontManagerRegisterGraphicsFont`
  (process scope). The visual check catches a regression: LastResort draws
  every private-use icon with one shared glyph id. (`get_font_for_id` can't
  name a font CoreText picked during fallback; it returns `None`.)
- **Modifier changes travel the focus path only.** A `ModifiersChangedEvent`
  goes from the focused node up to the root, so a listener registered in a
  custom element's paint (a child of the focused view's div) never hears it.
  The terminal registers `on_modifiers_changed` on the pane's own div.
- **Modifiers don't say which side.** `Modifiers::alt` is either Option key;
  `desktop.option_as_meta: left|right` reads the device-dependent bits
  (`NX_DEVICELALTKEYMASK` 0x20, `NX_DEVICERALTKEYMASK` 0x40) from
  `CGEventSourceFlagsState` at key-down instead.
- **The Option key and dead keys.** A key-down the element leaves unhandled
  goes to the platform input handler, which is where macOS composes dead keys
  (`setMarkedText("´")`, then `insertText("é")`) and IME text. So the terminal
  encodes a character key itself only when it has a control meaning (Ctrl, or
  Option while Option is Meta); everything else must stay unhandled. Option
  with a named key (Enter, arrows, Backspace, Delete) has no dead-key meaning
  and is always encoded as Meta, whatever `desktop.option_as_meta` says.
- **AccessKit is live only while an assistive technology is connected.**
  `Window::is_a11y_active()` gates the work; a node needs both an id and a
  role; `div().a11y_synthetic_children(..)` adds text runs under a node that
  has real keyboard focus (its `track_focus` registers it as focusable). The
  test platform never activates AccessKit, so the visual check can't see the
  tree.
- `simulate_keystrokes("alt-b")` in a `VisualTestAppContext` runs no input
  method: a key the app leaves unhandled types nothing. The visual check
  drives IME and dead-key text through the `EntityInputHandler` methods
  directly, which is the path macOS calls.
- **`cx.listener` borrows the context** (`dt-review-view`): a closure that
  builds rows with `cx.listener(..)` holds `cx` immutably for as long as the
  closure lives, so render any child that needs `&mut cx` (another
  `render_*` helper) before defining the closure.
- **A clamped paragraph needs an explicit ellipsis.** `line_clamp(n)` alone
  cuts the last line mid-word with no marker; add `text_ellipsis()` beside it.
  For a path, `text_ellipsis_start()` with `overflow_hidden().whitespace_nowrap()`
  keeps the end visible (the TUI's left-truncated worktree).

### Menus and system notifications (`dt-menus-notifications`, gpui-pre 0.3.9)

- **System notifications are in the pinned snapshot**: `App::show_system_notification`,
  `dismiss_system_notification` and `on_system_notification_response` (a
  `SystemNotification { tag, title, body, actions }`), over
  `UNUserNotificationCenter` in `gpui_macos::system_notifications`. No
  `objc2` bridge of our own was needed. The center is created lazily, and
  only when the main bundle has an identifier: under `cargo run` (no
  `CFBundleIdentifier`, see "Unbundled builds" above) posting is a logged
  no-op, so notifications only appear from `scripts/bundle.sh`'s `Shelbi.app`.
  Authorization is requested on the first post, not at launch. gpui's
  delegate presents banners even while the app is frontmost
  (`willPresentNotification` returns Banner | List), so suppressing a notice
  for what is on screen is the app's job (`src/notify.rs`). A notification's
  tag is its request identifier: posting the same tag replaces it.
- **Menu items are enabled by action availability, not by `disabled`.**
  AppKit autoenables menu items through `validateMenuItem:`, which gpui
  answers with `App::is_action_available` (a handler for the item's action
  on the active window's focused path, or a global one). `MenuItem::disabled`
  sets `setEnabled:`, which autoenabling overrides. So a disabled item gets an
  action nothing handles (`menus::MenuUnavailable`), and an item that should
  gray out with its target (Edit ▸ Copy without a terminal selection) has its
  handler registered conditionally (`.when(..., |d| d.on_action(...))`).
  Availability reads the last rendered frame.
- **Key equivalents come from the keymap.** `set_menus` takes each item's key
  equivalent from `Keymap::bindings_for_action`: the *earliest* binding whose
  context predicate matches a fixed Workspace/Pane/Editor context, else the
  earliest binding at all. A binding in a context no element declares is
  therefore a pure menu label: it never fires from the keyboard but still
  shows on the item. The desktop binds its `keys.yaml` shortcuts that way
  (context `ShelbiMenuLabel`) and keeps handling the keys itself. gpui has no
  way to remove one binding (`clear_key_bindings` drops gpui-kit's too), and
  data actions compare by value, so each new shortcut set is bound under a
  fresh `MenuInvoke::generation`; stale bindings match no item. Only one-
  keystroke bindings become key equivalents.
- **Key bindings dispatch before key listeners.** A keystroke matching a
  binding on the focused path runs its action first; `on_key_down` /
  `capture_key_down` only see it if no binding handled it. The Edit keys
  (⌘C ⌘V ⌘A) are bound in the terminal's `Terminal` key context, so they run
  the terminal's Edit actions there and fall through to the overlays' and
  gpui-kit inputs' own handling elsewhere.
- **A key the window handles never reaches the menu.** For a key-down with
  modifiers, AppKit offers the event to the key window's
  `performKeyEquivalent:` first and to the main menu only if gpui reports it
  unhandled, so a chord both the window and a menu item carry fires once.
- **The menu named "Window" is AppKit's Windows menu** (gpui calls
  `setWindowsMenu:` for it): AppKit appends the open windows by title, the key
  one checked, so the project windows are listed without our own items.
- `window.dispatch_action(..)` defers to the next effect cycle, so an action
  handler can forward a different action to the focused element (the window
  forwards Edit ▸ Copy to a focused gpui-kit input as
  `gpui_kit::base::input::Copy`).

- **`TextSystem::all_font_names()` is slow** (`dt-terminal-perf-multiclient`):
  it enumerates every installed font through CoreText, about 125 ms on the
  dev machine. The terminal checked its configured family against it on every
  new pane's first frame, so every session shown (a workspace switched to)
  stalled the UI thread that long. `CellMetrics::cached` measures once per
  family and size.
- **`cx.notify()` only repaints windows that rendered the entity last
  frame** (`App::notify` filters through `tracked_entities`). A terminal pane
  that is alive but not in the tree (hidden) can keep notifying for free; it
  costs no frames. The flip side: anything that *is* in the tree and notifies
  repaints the whole window (views aren't cached unless `.cached()`), so a
  feed that notifies on every unchanged read repaints the window on its timer.
- **Inactive windows are throttled to 30 fps** (`WindowOptions::
  inactive_frame_interval`, default 33 ms), but only frames forced by
  `on_next_frame` / animation requests; plain `notify` frames in an inactive
  window still run at the display rate. The terminal never uses
  `on_next_frame`, so a background window streams output at full rate.
- **`Window::dispatch_keystroke` inside `WindowHandle::update`** panics with
  "cannot update <root> while it is already being updated" when dispatch
  reaches the root view's listeners. Dispatch through
  `AsyncApp::update_window` (the window, not the root entity) instead (the
  bench's `--bench-client`).
- **Profiling a release build**: the release profile strips symbols, so
  `sample` shows addresses only. Build a symbolized copy in its own target
  directory: `CARGO_PROFILE_RELEASE_STRIP=none
  CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_LTO=false
  cargo build --release --features bench --target-dir target/prof`, then
  `sample <pid> <seconds>` while it runs (the main thread is
  `Thread_<n>: main`).

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
