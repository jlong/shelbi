# Phase 0 spike: emulator replay and the emulator-crate decision

Task `rt-spike-emulator-replay`. Prototype: `spikes/remove-tmux/replay/`
(outside the cargo workspace). Plan sections: "Emulation", "Attach replay",
Phase 0 item 3.

## Decision

**Vendor `alacritty_terminal` (0.26.0, Apache-2.0) as the client- and
session-side terminal emulator.** It is vendored under
`vendor/alacritty_terminal/` with a small read-only fork for replay; see
`vendor/alacritty_terminal/VENDORING.md`.

The deciding requirement (plan, "Emulation") is full access to **both screen
buffers, both saved cursors, the scroll region, tab stops, charsets, every mode
including the kitty keyboard-protocol stack, and history**, because replay
serializes all of it. Only `alacritty_terminal` models all of it. The reason it
must be *vendored* rather than used from crates.io is that it keeps most of that
state private; vendoring lets us expose exactly what replay reads (and pins a
pre-1.0 crate against its own breaking releases).

## Candidates against the deciding requirement

Verified by reading the 0.26.0 / vt100 0.16.2 sources and by the prototype.

| Replay needs | `alacritty_terminal` 0.26 | `vt100` 0.16 |
| --- | --- | --- |
| Active screen buffer | yes (`Term::grid()`) | yes (`Screen`) |
| **Inactive screen buffer** (screen under a full-screen program) | held in `Term::inactive_grid` (private; fork exposes it) | held in `alternate_grid` (private; **no** serialization path) |
| Scrollback / history | yes (`Grid` history) | yes (0.16 added it) |
| Saved cursor(s) | yes, per grid (`Grid::saved_cursor`, public) | partial, via `state_formatted` |
| Scroll region | `Term::scroll_region` (private; fork exposes it) | not exposed |
| Tab stops | `Term::tabs` (private; fork exposes it) | not exposed |
| Charsets | `Grid::cursor.charsets` (public) + `active_charset` (fork) | not exposed |
| **Kitty keyboard protocol** (Claude Code's Shift+Enter) | yes (`TermMode` flags + `keyboard_mode_stack`) | **absent entirely** |
| Serialize-the-whole-thing | not built in; needs the fork + a Phase 1 serializer | `contents_formatted()` serializes the **visible** screen only |

`vt100` is attractive because `Screen::contents_formatted()` serializes a screen
for free. But it is a **single-buffer** snapshot: it cannot carry the inactive
grid (so it cannot survive quitting a full-screen program, see case (b)), and it
has **no kitty keyboard protocol** at all, which the plan requires. Its ecosystem
is also fragmented into many partial forks (`panoptes-vt100`, `fnug-vt100`,
`atuin-vt100`, `shpool_vt100`, `term-wm-vt100`, `vt100-ctt`, …), each bolting on
one missing capability — a signal that no single `vt100` crate is complete.

`alacritty_terminal` is pre-1.0 and ships breaking releases (API-stability risk).
Vendoring converts that external churn into a deliberate, reviewed upgrade of our
fork; Zed vendors a fork for the same reasons.

## What the prototype does

Two independent emulators: `A` (the "session", fed a real output stream) and `B`
(the "client", built only from a replay byte stream regenerated from `A`'s
observable state). If `A` and `B` render identically, the state survived the
round trip. Fixtures under `tests/fixtures/` are **real PTY recordings** (an 80x24
PTY) made by the `capture` bin: `nvim` on a highlighted file, and an interactive
shell with markers echoed before `nvim` is opened over it. Tests are
deterministic (they replay the recorded bytes). Run:

```
cargo test  --manifest-path spikes/remove-tmux/replay/Cargo.toml
cargo test  --manifest-path spikes/remove-tmux/replay/Cargo.toml -- --nocapture report_metrics
cargo run   --manifest-path spikes/remove-tmux/replay/Cargo.toml --bin capture -- <out> shell-nvim
```

The replay serializer here is a stand-in: it regenerates escape sequences for the
visible cells (chars, colors, core SGR attributes), both buffers, scrollback, and
the cursor. The production serializer (both buffers, **saved cursors**, scroll
region, tab stops, charsets, the keyboard-mode stack, and a parser-reset) is the
largest single piece of Phase 1 (`rt-replay` / `rt-term`) and is out of scope
here. The spike's job is to prove the inputs are all *reachable* and that the
round trip reproduces a real screen.

## Case results

### (a) Reattach at unchanged size to a running full-screen program — WORKS

`nvim` open on a syntax-highlighted file (alternate screen, 256-color). `A` is
fed the recording; the serializer regenerates a replay; `B` is built from the
replay only. `B`'s visible screen matches `A` **cell-for-cell** (character, fg,
bg, and bold/dim/italic/underline/inverse/hidden/strikeout), plus cursor position
and visibility. Test: `case_a_fullscreen_roundtrip`.

*Not covered (fixable):* exotic cell attributes the stand-in serializer collapses
— undercurl / double / dotted / dashed underline map to plain underline, and
`CellExtra` (combining chars, hyperlink IDs, underline color) is dropped. All are
present in the emulator and reachable; the Phase 1 serializer carries them (fix:
serialize the full `Flags` and `CellExtra`, not the reduced set).

### (b) Attach while the program is open, quit it, screen underneath intact — WORKS (needs the fork)

Shell prints `SHELL_UNDERNEATH_MARKER` on the normal screen, then `nvim` opens
(alternate screen). The recording stops while `nvim` is open. Replay rebuilds
`B`'s normal screen *and* alternate screen; both emulators are then sent
`ESC[?1049l` (the program exits / leaves the alternate screen). Both reveal the
shell markers, matching cell-for-cell. Test:
`case_b_quit_reveals_shell_underneath`.

This is the case that forced the crate decision: it requires reading the inactive
grid, which upstream `alacritty_terminal` keeps private. The fork adds a
read-only `Term::inactive_grid()` (plus accessors for the scroll region, tab
stops, charset, and both keyboard-mode stacks). **Without the fork this case is
impossible** against the stock crate's public API.

The earlier draft's "send a resize to force a repaint" idea is confirmed
unreliable and unnecessary (plan, "Attach replay"): at unchanged size a program
may redraw nothing, and it never rebuilds the normal screen underneath. Replay
reconstructs state directly instead.

*Comparison, measured:* feeding the same recording through `vt100` and replaying
via its own `contents_formatted()`, then leaving the alternate screen, the shell
markers are **gone** — vt100's single-buffer serialization cannot carry the
screen underneath. Test: `comparison_vt100_replay_loses_inactive_screen`.

### (c) Split the live stream only where the parser is at rest — WORKS

A small VT state tracker marks the offsets where the parser is at Ground with no
pending UTF-8 continuation. Rest-boundary framing reassembles losslessly and,
fed chunk-by-chunk, produces a screen identical to feeding the whole stream; no
chunk edge falls inside a sequence. Naive fixed-size framing, by contrast, tears
real output badly (measured on the `nvim` recording):

| Framing | Torn boundaries |
| --- | --- |
| fixed 8-byte | 297 |
| fixed 16-byte | 151 |
| fixed 32-byte | 75 |
| fixed 64-byte | 36 |
| **rest-boundary** | **0** |

The consequence of a torn edge is shown at the emulator level: a client that
rebuilt state up to a cut (parser reset to Ground) and then receives the tail
renders a clean red `B` when the cut is at rest, but leaks the SGR params as the
literal text `31m` when the cut lands inside `ESC[31m`. Tests:
`case_c_rest_boundary_framing_is_lossless`, `case_c_torn_sequence_corrupts_a_resuming_client`.

A single streaming parser buffers partial sequences across `advance()` calls, so
*arbitrary* chunking is safe for one long-lived emulator. The rest-boundary rule
matters specifically for **frame edges**: sequence-numbered output frames and the
replay/live split a reconnecting client resumes at.

## Summary: what works, what doesn't, and whether it's fixable

| Item | Result | Fixable? |
| --- | --- | --- |
| (a) full-screen round trip, cell-exact | works | — |
| (b) screen underneath survives a quit | works (via fork accessor) | — (needs the fork, done) |
| (c) rest-boundary framing lossless; torn edges corrupt | works | — |
| Exotic underlines / `CellExtra` in the stand-in serializer | dropped | yes — Phase 1 serializes full `Flags` + `CellExtra` |
| Write side: install serialized state (saved cursors, scroll region, tabs, charsets, keyboard stack) into a fresh `Term` | not built | yes — Phase 1 `rt-replay`; needs fork setters / a `from_state` constructor |
| Kitty keyboard protocol | present in the crate | — |

## Deliverable: emulator available to the workspace

`vendor/alacritty_terminal/` (Apache-2.0, `LICENSE-APACHE` retained) is a path
dependency in `[workspace.dependencies]`, added as a dependency of `shelbi-term`
so that crate and the session process can build against it. It is **not** a
workspace member (listed under `exclude`), so `cargo clippy --workspace
-- -D warnings` never lints upstream code and `cargo test --workspace` never runs
its suite. No product code uses it yet; `rt-term` wires the `emulator` seam to it.
Fork divergences are logged in `vendor/alacritty_terminal/VENDORING.md`.
