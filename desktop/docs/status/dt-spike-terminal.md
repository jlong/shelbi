# dt-spike-terminal: the terminal element

## What landed

- **`src/elements/terminal/`**, the terminal element:
  - `TerminalPane` (a gpui view) owns one session's `shelbi_term::view::TerminalView`.
    It connects off the UI thread (`session.rs`: discovery, retrying for 5s
    while a new session binds its socket), drains output from an async
    channel fed by `shelbi_client`'s reader thread, and writes input through a
    writer thread that owns the connection. No socket IO runs on the UI thread.
  - `TerminalElement` (a custom gpui `Element`) paints the grid: cell
    backgrounds merged into runs, glyphs shaped one run per style (single-width
    runs pinned to the grid with `force_width`, wide characters shaped alone),
    bold, italic, underline (curly as wavy), strikethrough, dim, inverse,
    hidden, the ANSI 16 from the theme, the xterm 256 cube and ramp, true color,
    and the program's OSC 4/10/11/12 overrides. It also paints the cursor
    (block with the glyph inverted, beam, underline, or an outline when
    unfocused, blinking only when the program asks, and only while focused, for
    10s after the last activity), the selection, search highlights (current
    match in `ACCENT`), and the IME composition.
  - **Damage-driven repaint**: the pump notifies only when a batch of output
    damaged the emulator, and `RowCache` rebuilds only the damaged rows. An idle
    redraw rebuilds 0 rows, and an idle session costs no CPU (measured below).
  - **Sizing**: the grid size comes from the element's pixel bounds and the
    measured cell (`shelbi_term::Size::from_pixels`). The first size is
    reported at once, later changes after 60ms without another change. The
    session applies its most-recently-active-client rule. While the session's
    size differs from the pane's, the grid is letterboxed or clipped with
    `shelbi_term::viewport::fit`, as in the TUI.
  - **Keys** (`keys.rs`): gpui keystrokes are classified, then encoded by
    `shelbi_term::input::encode_key` (the TUI's encoder). Typed text falls
    through to the pane's `EntityInputHandler`, so macOS IME composition and dead
    keys work. Shift+Enter is `CSI 13;2u` under the kitty protocol, Option is
    Meta for character keys, Ctrl+H is BS (Backspace is DEL), and arrows follow
    DECCKM. Cmd+C copies the selection, Cmd+V pastes as a `Paste` frame (the
    session brackets it when the program asked). Shift+PageUp/PageDown/Home/End
    page Shelbi's scrollback. Other Cmd chords go to the app.
  - **Mouse**: the TUI's gesture rules, now shared (below). A drag selects and
    copies, a click reaches a program that reports the mouse, Shift keeps a
    gesture Shelbi's, and Alt hands a drag to the program. The wheel scrolls
    scrollback unless the program owns the mouse, Shift+wheel always scrolls
    scrollback, and trackpad pixel deltas accumulate into whole lines.
  - Focus in/out reaches programs that enabled focus reporting (pane focus
    and window activation both count).
- **`--spike <session>`** (`src/spike.rs`): one window with one element on an
  existing session. It stays as a developer tool.
- **`--visual-check [dir]`** (feature `visual-check`, `src/visual_check.rs`): an
  offscreen end-to-end check through gpui's `VisualTestAppContext`, which
  renders with the real Metal renderer into a texture. It needs no screen and
  no screen-recording permission, and it works on a locked machine. It saves
  frames (showcase, search, selection, scrolled, IME, resize), drives gpui key
  and mouse events through the real dispatch path into a recording fake
  session, checks the bytes (38 checks), and times frames.
- **`shelbi-term` additions**, plain data with tests:
  - `render`: `Cursor`/`CursorShape`, `Damage`/`RowDamage`, `CellAttrs`/`Underline`, `Rgb`, `xterm_256`.
  - `TermEmulator::{cursor, take_damage, color_override, key_encoding, mouse_modes}`, `TerminalView::{take_damage, search_matches}`, `Size::from_pixels`.
  - `gesture::MouseGesture`: the TUI's left-button drag/click state machine plus
    the wheel and other-button policy, moved out of `shelbi-tui` so both
    clients run one copy. The TUI's `TerminalPane::on_mouse` now delegates to
    it, `key_encoding` uses the emulator's, and `cell_style` reads `CellAttrs`.
    No TUI behavior changed (its mouse and terminal tests pass unchanged).
- `theme.rs`: `TerminalPalette`, resolved with the theme (dark and light,
  following the window appearance as `dt-theme-tokens` set up). The default
  foreground, background, cursor, selection, and current-match colors come from
  the shared tokens (`foreground`, `canvas`, `white`, `selection`, `cyan`). The
  ANSI 16 are Ghostty's defaults in dark (the palette the TUI tokens were tuned
  against) and a GitHub-light style set in light. The spike window follows
  light/dark switches.

## Decisions

- **Stay on `gpui-pre` + `gpui-kit`** (the foundation's choice). The element
  builds unchanged against a pinned Zed git dependency at the same commit;
  details and numbers are in `../gpui.md`. The orchestrator decides.
- **Shared gesture code over a desktop copy.** The task's mouse summary
  ("forwarded when reporting, Shift is Shelbi's") is the old per-event policy.
  The TUI's actual behavior since `rt-mouse-select-copy` is richer (plain drag
  selects, Alt+drag forwards). Moving the state machine into `shelbi-term`
  keeps the two clients identical.
- **Copy on finished drag**, as the TUI does, plus Cmd+C.
- **Option is Meta** for character keys (`OPTION_AS_ALT`), because Claude
  Code's Option chords need it. Option+letter characters (`∫`) are then not
  typeable; `dt-terminal-complete` makes it a setting.
- **Text through the input handler, chords through key events**, so IME and dead
  keys compose in the platform's own path and the encoder sees only keys with a
  control meaning.
- **Geist Mono when installed, else Menlo.** The app doesn't bundle Geist Mono
  yet (`dt-terminal-complete` / `dt-packaging` should bundle it via
  `text_system().add_fonts`). Ligatures are off so glyphs stay one per cell.
  Cell height is ascent + |descent|, at least 1.25x the font size (13px gives
  7.8 x 17 px cells). gpui's font-metric descent is signed, which first gave
  10px rows.
- **The cursor isn't baked into rows.** alacritty always reports the cursor's
  row as damaged. The pane drops that cursor-only damage at frame start, so an
  idle redraw rebuilds nothing; output damage is taken by the pump.
- **Feeding happens on the foreground task**, in batches of at most 256 KiB
  per turn with a yield between, so a flood interleaves with frames and input.
  Parsing costs ~1.4–2.6 ms per 64 KiB; see the numbers below.

## Measurements

Apple M4 (10 cores). **The hub was heavily loaded** (load average 54–63 from
other workers' builds) throughout, so wall-clock numbers are pessimistic and
noisy. The frame numbers are CPU time in our element.

Live app (`--spike`, release build):

| Case | Result |
|---|---|
| Idle, attached to Claude Code | 0.01 s CPU over 60 s (≈0.02%), RSS 111 MB, no frames painted |
| `yes \| head -n 3000000` (live session, 128x41) | 35–47 fps painted, frame avg 100–119 µs, max 170 µs, all 41 rows rebuilt per frame; parse ≈10 ms per 2 s window |
| Same flood, no client attached | 48 s in the session alone vs ≈44 s with the spike attached: the session process (a debug `shelbi`) is the ceiling, not the client |

Offscreen (`--visual-check`, release build, 128x35 grid):

| Case | Result |
|---|---|
| Idle redraw | avg 131 µs, p95 160 µs, 0 rows rebuilt |
| 3 MB `yes` flood, a frame per 64 KiB chunk | frame avg 49 µs, p95 70 µs, max 125 µs |
| Parse, 64 KiB chunk (emulator alone) | p50 1.4–2.8 ms; p95 13–165 ms spikes are scheduler preemption under the load (they move between runs) |

## Verification

Rebased (2026-10-14) onto `jlong/desktop-app` at `41db6920` (`dt-shell-sidebar`): `main.rs` keeps the shell's launch (positional project argument, project windows, `gpui_kit::init`, `theme::init`, fonts, menu and key bindings) and adds `--spike` (opens before the catalog/bootstrap globals, so it bypasses the project shell) and `--visual-check`; `Cargo.toml` merges both dependency sets. Re-ran everything below on the rebased tip: desktop build/clippy (both feature sets)/`cargo test` (118 passed), `--visual-check` 38/38, `--spike` opens with no shell, and root `cargo check --workspace --locked`, `build`, `clippy -D warnings`, `cargo +1.88 check --all-targets --locked` and `cargo test --workspace` (no failures) all ok.

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok
- `cargo clippy --all-targets --features visual-check -- -D warnings`: ok
- `cargo test`: 92 passed (the terminal's: key classification for every
  chord above, cell metrics → `Size`, pixel → cell mapping, row layout for
  attributes, colors, and wide/emoji/combining cells, the damage-driven cache,
  and the palette following the theme; plus the foundation's, the platform,
  theme, and Issues board tests already on the umbrella)
- `cargo run --features visual-check -- --visual-check <dir>`: 38/38 checks
  pass (keys, kitty/DECCKM modes, UTF-8 text, paste plain and bracketed,
  IME compose/commit, wheel and drag ownership with and without mouse
  reporting, Shift overrides, debounced resize report, idle redraw)

From the repository root (`shelbi-term` and `shelbi-tui` changed):

- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo check --workspace --locked`: ok; the root `Cargo.lock` is unchanged
- `cargo +1.88 check --workspace --all-targets --locked` (the CLI's MSRV): ok
- `cargo test --workspace --no-fail-fast`: 3889 passed, 0 failed (49 test
  binaries), on the final tree (rebased onto `dt-theme-tokens` and
  `dt-issues-board`). One earlier run (before
  that rebase) failed `remote_session_e2e::remote_dispatch_to_handoff_reconnect_and_unreachable`,
  a 10s wait for a stub's ready screen over the relay, at load ~65. It passed
  on the next full run. Nothing it exercises uses `shelbi-term`.

By hand (`--spike`, screenshots only, no synthetic input because the machine
was in use): a zsh prompt (colors, bold, the cursor), Claude Code's start
screen (OSC title reaching the window title, logo, colors, dim text, inverted
block-cursor glyph), the connect retry (spike started before its session),
and the idle and flood measurements above.

## What's left

- **Not driven by hand in the live app:** typing, Shift+Enter, scrolling,
  window resizing, and Shift+drag against a real Claude Code session. The
  machine was locked overnight and in use afterwards, so the live checks were
  screenshot-only. The same paths are driven in `--visual-check` through
  gpui's real dispatch (into a fake session), and the encoder bytes are
  unit-tested. A human pass with `--spike` on `claude` is the remaining
  acceptance step.
- **For `dt-terminal-complete`:** box-drawing and block elements are drawn as
  font glyphs, so with 17px rows the verticals and block rows show hairline
  gaps (visible in Claude Code's logo); they should be drawn as geometry. ZWJ
  emoji sequences render as separate people (alacritty gives each its own wide
  cell). The bell is ignored. Alternate-scroll mode (wheel → arrows on the
  alternate screen) isn't implemented. There's no search UI (painting and
  `TerminalView` search work). Geist Mono isn't bundled. Option-as-Meta should
  be a setting. Selection auto-scroll at the edges is missing. IME marked text
  is drawn over the cells at the cursor rather than inline.
- **For `dt-terminal-perf-multiclient`:** parsing runs on the UI thread in
  bounded batches. That's fine at the session's real output rate, but a
  faster (release) session process could push enough output to make an
  off-thread emulator worth it.
