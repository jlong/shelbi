# rt-tui-shell — In review

Phase 4b: the single-process TUI shell and its terminal view. One ratatui
program that owns the whole screen, behind the hidden `session_backend` dev
flag. With the flag off, nothing changes (the tmux `exec attach` path is
untouched).

## What landed

New module `crates/shelbi-tui/src/shell/` (a renderer over `shelbi-app` — no
model logic added to the TUI crate):

- **Entry (`lib.rs`).** `run_main` now branches after `ensure_dashboard`: with
  `session_backend_enabled()` it `return shell::run(project)` instead of
  `exec tmux attach`. `ensure_dashboard` already brings up the orchestrator
  *session* (not a tmux dashboard) under that flag, so the shell creates no
  tmux session, window, or pane.
- **One event loop (`shell/mod.rs`).** Local input, session output, model
  snapshots, and timers in a single loop. Input is drained non-blocking; the
  session output channel and the refresher are polled each tick; the loop
  paces on `event::poll(16ms)`. Redraws are capped at ~60/s and wrapped in
  crossterm `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`.
- **Layout.** Sidebar left, main area right. The sidebar renders over
  shelbi-app's `SidebarModel` (built by `spawn_refresher`), reusing the
  existing sidebar's labels/glyphs (💬 Chat, 📋 Issues, ⚡ Activity) and
  `theme::SELECTION_BG`. Navigation state (selection, focus, sidebar width) is
  shelbi-app's `ClientState`.
- **Terminal view (`shell/terminal_view.rs`).** A ratatui widget over
  `shelbi_term::view::TerminalView` + `shelbi_client`: attach with replay,
  live output, resize, clip/letterbox via `viewport::fit`. Mirrors the
  `shelbi session attach` render/input path and adds a **truecolor → xterm-256
  downgrade** for terminals/multiplexers without 24-bit color.
- **Off the UI thread (`shell/session.rs`).** Connecting + attaching to a
  session (socket I/O, `info`, replay) runs on a worker thread and reports back
  over a channel; the loop only polls. A blocked connect never freezes the UI
  (tested with a blocking stub connector). Scrollback is held only for the one
  session being viewed.
- **Focus, keys, mouse.** No prefix, no modes. With the terminal view focused
  every key reaches the agent except **Ctrl+Space**, which (until
  `rt-tui-overlays`) moves focus to the sidebar. Tab/arrows navigate the
  sidebar; Enter opens the selection; a click focuses+selects. Mouse ownership
  follows `input::mouse_owner`: program mouse is forwarded with coordinates
  translated against the **draw-area** size (`viewport::fit`), so a click lands
  on the right session cell even when the session is letterboxed or clipped
  (and a click in the margin / past a clipped window maps to nothing);
  Shift+wheel/drag (and plain wheel/drag when the program hasn't asked for the
  mouse) scroll Shelbi's scrollback and select. Selection copies via **OSC 52**
  with a `pbcopy` fallback on macOS.
- **Scrollback / search.** On the normal screen only (`TerminalView` enforces
  it). Shift+PageUp / wheel enter scrollback; in scrollback `/` opens an inline
  search prompt, `n`/`N` cycle matches, Esc returns to the live bottom.
- **Nesting (`shell/caps.rs`).** Runs as an ordinary full-screen program inside
  tmux/Screen (no `$TMUX` branching). Detects the missing kitty-keyboard
  round-trip (`supports_keyboard_enhancement`) and the outer multiplexer
  (`$TMUX` / `$STY`), and shows the one-line notice **once** tailored to the
  mitigation that applies (tmux → `set -s extended-keys on`; Screen → run
  outside Screen; otherwise a generic note). Truecolor falls back to 256.
- **Sizing.** The shell reports the main-area size so the session reflows to
  fill it when the shell is the most-recently-active client; otherwise
  `viewport::fit` clips or letterboxes.

Deps added to `shelbi-tui`: `shelbi-app`, `shelbi-term`, `shelbi-client`,
`shelbi-proto`, `alacritty_terminal` (all workspace).

## Deferred (named phases)

- Native views (Issues/Machines/Activity main area) are placeholders —
  `rt-tui-native-views` (4c).
- Overlays (palette, review confirm/reject, error log, zen intro) and a rich
  search/selection UX — `rt-tui-overlays` (4d).
- Review interface (panel + editor/diff sessions) — `rt-tui-review` (4e).
- Quit actions / project switching — 4f (a minimal `q` closes the UI from the
  sidebar for now).
- The richer tmux-sidebar features not in `SidebarModel` (machine groups,
  legacy spawned agents, queued-vs-ready split, version/zen footer) are a
  follow-up to extend `shelbi-app`'s `SidebarModel`; the shell renders what the
  model carries.

## Rework (2026-10-04): verification added

The review passed on architecture/scope; these criteria gained the verification
the task asked for.

- **AC4 — real-PTY key echo.** `shell/pty_input_tests.rs` spawns a genuine
  `shelbi_session::run` on a thread with a `stty raw -echo; exec cat` child (the
  `protocol_e2e.rs` pattern), binds a real `ShellState` to it, and drives the
  actual event-loop input function (`handle_key` with crossterm `KeyEvent`s)
  with letters, Ctrl+C, Ctrl+], Esc, arrows, an F-key, Alt+x, and — after
  enabling the kitty protocol in the pane — Shift+Enter. An independent observer
  client reads the echoed bytes and asserts the agent received exactly the
  encoder's output (incl. `ESC[13;2u` for Shift+Enter), and that **Ctrl+Space
  never reached it** (no NUL in the stream; focus moved to the sidebar).
- **AC5 — mouse translation.** New `terminal_view` tests: a program-owned click
  at a viewport cell resolves to the correct session cell when letterboxed
  (80×24 in 100×30 → SGR `ESC[<0;6;3M`) and when clipped (120×40 in 80×24), a
  click in the letterbox margin forwards nothing, and Shift+drag produces a
  Shelbi selection rather than forwarding even while the program is reporting
  the mouse. This drove a fix: `on_mouse` now takes the draw-area size and
  builds the placement from it (it previously used the emulator size, i.e. an
  identity placement that ignored the letterbox/clip offset).
- **AC6 — OSC 52.** A completed selection ("hello") yields
  `ESC ] 52 ; c ; aGVsbG8= ST` on the copy path.
- **AC9 — notice once.** `caps` detection is now `$TMUX`/`$STY`-aware and the
  notice is a one-shot: `ShellState::notice_text(now)` shows it until its
  deadline then clears it for good (driven deterministically with injected
  instants), so it is emitted exactly once per run. Detection-level test per the
  rework note; no real tmux needed.

## Manual check — real agents in a session (rework 2026-10-04)

Confirmed on macOS (Darwin 25.6, arm64) with the built binary, flag on, via
`shelbi session new … -- <agent>` + the throwaway raw-input helper +
`shelbi session snapshot` (no client attached — the session process answers
startup queries alone):

- **Claude Code 2.1.289** (Opus 5.5): booted to its trust-folder prompt, then
  (after selecting "Yes, I trust this folder") to its composer. Sending
  `AA`, Shift+Enter (`ESC[13;2u`), `BB` left the composer holding

  ```
  ❯ AA
    BB
  ```

  — Shift+Enter inserted a newline and did **not** submit.
- **Codex 0.160.0** (GPT-6-Astra): booted to its `› Ask Codex to do anything`
  composer. The same `AA` / Shift+Enter / `BB` sequence produced `› AA` then
  `BB` on the next line, again a newline without submitting.

Both reached their input box and both treat the shell's Shift+Enter encoding as
a newline, confirming AC3 end to end against the real agents (the byte delivery
itself is now also covered headlessly by the AC4 real-PTY test). OSC 52 copy to
the OS clipboard still needs a human at a real terminal to confirm the paste
buffer; the sequence emitted is asserted by AC6.

## Config-upgrade

No shipped `*.template` / default config / workflow / instructions file
changed (new in-code module + crate deps, gated by the existing
`session_backend` dev flag), so no config-upgrade sniffer is needed — same
reasoning as `rt-backend-sessions`.

## Tests

`shelbi-tui` unit tests under `shell::` (31): truecolor passthrough +
RGB→256 anchors, inverse swap, Ctrl+Space reservation, Shift+Enter kitty
encoding, plain Enter = CR, wheel scrollback, scrollback search, sidebar
selectable-row routing + click hit-testing, a blocked connect never freezing
the UI, same-target no-op, layout split/clamp, release-key filtering, and
ShellState focus toggle + sidebar navigate/activate routing; plus the rework
additions — the real-PTY key-echo e2e (AC4), program-mouse letterbox/clip
translation and Shift+drag selection (AC5), the OSC 52 copy sequence (AC6),
`$TMUX`/`$STY` nesting detection with tailored notices, and the
notice-emitted-exactly-once latch (AC9). The AC4 test is `#[cfg(all(test,
unix))]` and silently needs `/bin/sh` + a PTY, like the other session e2es.
