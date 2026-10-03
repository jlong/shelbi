# rt-spike-agents findings

Phase 0 spike: can Claude Code and Codex run in a Shelbi-owned PTY, rendered by
a ratatui widget, with no tmux? And can the Codex three-process orchestrator run
the same way?

Prototype: [`spikes/remove-tmux/agents/`](../../../spikes/remove-tmux/agents/)
(standalone crate, excluded from the cargo workspace). Build and run:

```sh
M=spikes/remove-tmux/agents/Cargo.toml
cargo test  --manifest-path $M            # 25 deterministic encoder/emulator proofs
cargo build --release --manifest-path $M
BIN=spikes/remove-tmux/agents/target/release/rt-spike-agents
$BIN probe --cmd codex  --secs 6          # boot codex in a PTY, responder on
$BIN probe --cmd codex  --secs 6 --no-answer   # same, responder muted (A/B)
$BIN probe --cmd claude --secs 6
$BIN bench --mb 32 --rows 50 --cols 200   # redraw cost under heavy output
$BIN attach --cmd codex                   # interactive render (needs a real TTY)
```

The harness owns a `portable-pty` PTY, runs a byte-stream **query responder**
that answers terminal interrogations onto the PTY master, feeds output into a
`vt100` grid, and (for `attach`) paints that grid with a ratatui widget and
forwards decoded keys/mouse/paste back. It strips `TMUX`/`TMUX_PANE`/`STY` from
the child so every result below is from a bare PTY with no multiplexer present.

Environment exercised: macOS (Darwin 25.6, arm64), `codex-cli` 0.160.0, `claude`
(Claude Code) 2.1.288, `portable-pty` 0.9.0, `vt100` 0.16.2, `ratatui` 0.28.

> `vt100` is used here only as *a* grid to render and to count columns. It is
> **not** the emulator-crate decision, which is `rt-spike-emulator-replay`
> (`alacritty_terminal` vs. a `vt100`-family crate). Nothing here commits the
> production crates to `vt100`.

## Summary

Both agents boot and run in a bare Shelbi-owned PTY with no client attached, as
long as the PTY owner answers startup queries. The redraw path is far cheaper
than any frame budget. Every input-encoding behavior the plan names, including
Shift+Enter under the Kitty keyboard protocol, mouse forwarding, bracketed
paste, and wide-character layout, is implemented and unit-tested. The Codex
three-process orchestrator needs no `$TMUX_PANE` and no duplicated stdin; those
are artifacts of the tmux pane wrapper, and the spike reproduces the exact
process shape it depends on. The one thing not exercised is a human driving the
interactive `attach` render and visually confirming glyphs; that path compiles
and its encoder/emulator are tested, but live confirmation is a follow-up.

## The startup query responder (Phase 0 item 2)

This is the central risk. In the target model the `shelbi __session` process
spawns an agent into a PTY it owns with **no rendering client attached yet**. A
full-screen agent's first act is to interrogate the terminal. tmux answers these
today because it owns the PTY; once Shelbi owns it, Shelbi must answer, from the
session process, whether or not a UI is attached.

The responder ([`responder.rs`](../../../spikes/remove-tmux/agents/src/responder.rs))
is a pure byte-stream scanner, independent of the render emulator. It handles,
with unit tests for each and for the split-across-reads case:

| Query | Reply |
| --- | --- |
| DSR cursor position `CSI 6 n` | `CSI <row> ; <col> R` from the live emulator cursor |
| DSR status `CSI 5 n` | `CSI 0 n` |
| Primary DA `CSI c` / `CSI 0 c` | `CSI ? 62 ; 22 c` |
| Secondary DA `CSI > c` | `CSI > 1 ; 9500 ; 0 c` |
| XTVERSION `CSI > 0 q` | `DCS > | rt-spike-agents(0.0) ST` |
| Kitty keyboard flags `CSI ? u` | `CSI ? <flags> u` |
| OSC 10/11 color `?` queries | `rgb:...` (sets are ignored) |

It also tracks the Kitty flag stack the agent pushes/pops (`CSI > f u`,
`CSI < n u`, `CSI = f ; m u`) and exposes the active flags to the input encoder,
because Shift+Enter fidelity depends on them.

**Results, all with no client attached:**

- **Synthetic agent** (`synth`, built in): emits all seven query types plus mode
  sets. The harness detected and answered all seven on a real PTY child, and the
  emulator observed alternate-screen, bracketed-paste, mouse, and the pushed
  Kitty flag. This proves the responder end to end without needing network or
  auth.
- **Codex** asks, at startup: DSR cursor position, OSC 10 and 11 colors, the
  Kitty keyboard query, and DA1 (5 queries). **With the responder on it boots
  fully**: alternate screen, bracketed paste, mouse reporting, Kitty
  disambiguate flag all enabled, ~50 KB of TUI painted, alive indefinitely.
- **Claude Code** asks: XTVERSION (x2), the Kitty keyboard query, and DA1 (x4).
  It boots to its inline prompt, enables bracketed paste, and pushes the Kitty
  disambiguate flag.

**Is the responder necessary?** A/B with `--no-answer` (identical read loop, no
replies written):

- **Codex**: output collapses from ~50 KB to ~4.7 KB. It does **not** exit in
  0.160.0; it **stalls** with a blank or partial frame, blocked waiting for the
  replies. The plan's phrasing ("Codex exits if the cursor-position reply is
  late") is version-sensitive: this build hangs rather than exits. Either way
  the conclusion is the same and firm: **Codex is unusable without the
  responder.**
- **Claude Code**: roughly unchanged (~2.1 KB both ways). It degrades
  gracefully rather than stalling, and notably it pushes the Kitty disambiguate
  flag optimistically, visible in the output stream even when the query goes
  unanswered. So Claude does not depend on the reply to stay alive, but see
  Shift+Enter below for why answering still matters.

**Verdict:** works. The session process must run this responder against its own
headless emulator (for the live cursor position) from the moment it spawns an
agent. This is a hard requirement, not an optimization, and it is small.

## Per-agent, per-check results (Phase 0 item 1)

"Tested" below means a deterministic unit test in the harness
([`input.rs`](../../../spikes/remove-tmux/agents/src/input.rs),
[`term.rs`](../../../spikes/remove-tmux/agents/src/term.rs)); "observed" means
seen live in a `probe` run against the real agent.

| Check | Claude Code (2.1.288) | Codex (0.160.0) | Status |
| --- | --- | --- | --- |
| Boots, no client attached | Yes, inline prompt | Yes, full alt-screen TUI | works |
| Keys (chars, ctrl, alt, arrows, backspace, esc, tab) | encoder tested | encoder tested | works |
| Shift+Enter | pushes Kitty flag (observed); encoder emits `CSI 13;2u` (tested) | pushes Kitty flag (observed); encoder tested | works, with caveat below |
| Mouse forwarding + coordinate translation | mouse not enabled by Claude | enabled (observed); SGR 1006 encoder tested | works |
| Bracketed paste | enabled (observed); encoder tested | enabled (observed); encoder tested | works |
| Wide characters / emoji | CJK + emoji two-column layout tested | same | works |
| Redraw cost under heavy output | see bench | see bench | works, cheap |

### Keys and Shift+Enter

The input encoder decodes UI key events and re-encodes them for the agent
(the session process never passes raw input through, since a client may be on
any terminal or none). Covered and tested: printable chars, Ctrl (0x01..0x1f),
Alt (ESC prefix), the four arrows with and without modifiers (`CSI 1 ; m X`),
backspace (0x7f), Esc, Tab, and Shift+Tab (`CSI Z`).

**Shift+Enter** is the one real fidelity dependency. Without the Kitty
"disambiguate escape codes" flag, Shift+Enter is indistinguishable from Enter on
the wire (both are CR), so a newline-vs-submit distinction is impossible. Both
agents enable the Kitty protocol, so the flow works: the agent pushes the flag
(the responder observes it), and the encoder emits `CSI 13 ; 2 u` for
Shift+Enter when the flag is active (`CSI 13 ; 5 u` for Ctrl+Enter, etc.). The
test `shift_enter_needs_kitty` pins both the degraded fallback and the correct
Kitty encoding.

Caveat: the session process's input client must advertise and honor the Kitty
protocol end to end. That is a Phase 4 TUI requirement (`rt-tui-shell` input
model). The chord type is already slated to move off crossterm there; the Kitty
encoder in this spike is a drop-in reference for it.

### Mouse

Shelbi forwards the mouse only to an agent that asked for it (the decision
table). The emulator reports the agent's mouse-protocol mode, and the encoder
produces SGR 1006 reports (`CSI < b ; col ; row M/m`) with modifier bits and
drag/scroll kinds, tested. Coordinates are translated to the agent's 1-based
cell grid at the call site (in `attach`, from the ratatui mouse event). Codex
enabled mouse reporting live; Claude did not request it in this build.

### Wide characters and emoji

Fed through the `vt100` grid and checked by column accounting: a grinning-face
emoji and CJK ideographs each occupy two columns and advance the cursor by two,
with the trailing cell marked as a wide continuation; ASCII and accented Latin
(`café`) stay single-width. This is emulator behavior; the production emulator
decision (`rt-spike-emulator-replay`) must preserve it, and the replay spike's
grid comparison should include wide cells.

### Redraw cost under heavy output

`bench` spawns a burst child and measures (1) time to feed every byte into the
emulator and (2) time to paint a full off-screen frame, repeated.

| Grid | Burst | Parse throughput | Render / frame | Render ceiling | Peak RSS |
| --- | --- | --- | --- | --- | --- |
| 40x120 | 16 MiB | 115.5 MiB/s | 60.9 us | ~16,400 fps | 3.2 MB |
| 50x200 | 32 MiB | 117.6 MiB/s | 111.3 us | ~9,000 fps | 3.2 MB |

Render cost is O(cells) and trivial: even a full 50x200 frame repaints in ~0.1
ms, so at a 60 fps cap the widget uses well under 1% of the frame budget. The
real cost under a flood is **PTY read granularity**: a 32 MiB burst arrived in
~255,000 reads (~130 bytes each), so the session process (and any attached
client) must **coalesce reads and cap redraws** (for example, drain all pending
output, then render at most once per frame) rather than render per read. That is
a UI-loop design note for Phase 4, not a blocker. Parse at ~117 MiB/s bounds a
runaway agent comfortably.

## What does not work / was not exercised

- **Interactive `attach` with a human at the keyboard.** The render loop
  compiles and runs, and its encoder and emulator are unit-tested, but a person
  typing into a live agent and visually confirming glyphs, colors, and
  Shift+Enter was not done in this spike (the harness ran headless). This is the
  one item needing human-in-the-loop follow-up; it is low risk given the tests,
  and it belongs with the Phase 4 `rt-tui-shell` terminal-view widget.
- **Claude Code full-screen mode.** The task names a "default full-screen mode
  and inline mode." Claude Code 2.1.288 renders **inline on the normal screen**
  (it never entered the alternate screen in any run) and exposes no
  screen-mode flag in `--help`. This is actually favorable: Claude's output
  stays on the normal screen, so Shelbi's scrollback, selection, and search
  (the decision table's normal-screen features) apply to it directly. Codex is
  the alternate-screen full-screen agent, so its own scrollback is internal and
  Shelbi's normal-screen scrollback does not apply while it is up. Both modes
  the task meant are therefore covered by the Claude-inline vs. Codex-alt-screen
  pair; there is no separate Claude full-screen mode to test in this version.
- **Exhaustive emulator fidelity** (sixel, true-color gradients, combining
  marks, bidi). Out of scope here and owned by `rt-spike-emulator-replay`.

## The Codex orchestrator as a single session (Phase 0 item 4)

Today a Shelbi-managed Codex orchestrator is three processes in one tmux pane
(`crates/shelbi-orchestrator/src/wake.rs`, `lib.rs` around `:1521`/`:1697`):

1. **The bridge** (`shelbi __codex-orchestrator <project>`, `run_codex_bridge`):
   the pane's foreground process. Owns the app-server and the TUI, creates and
   persists the exact thread, and delivers board events via `turn/steer` /
   `turn/start` (it never pastes into the composer).
2. **`codex app-server --listen unix://<sock>`**: spawned by the bridge with
   `Stdio::null()` for stdin/stdout/stderr. A pure socket RPC server; **it needs
   no terminal at all.**
3. **The remote TUI** (`codex resume <thread> --remote unix://<relay>`): spawned
   by the bridge with `Stdio::inherit()`, so it draws to whatever terminal the
   bridge has.

### What it depends on, and why those are tmux artifacts

The two dependencies the task names both live in the **tmux pane wrapper**
(`orchestrator_pane_cmd`, `lib.rs:1697`), not in the Codex code:

- **Duplicated pane stdin** (`exec 3<&0; {launch} <&3`). The wrapper runs the
  bridge as a backgrounded job alongside a backgrounded heartbeat loop. With job
  control off, POSIX would hand a background job `/dev/null` for stdin, so the
  wrapper dups fd 0 to give the bridge (and through it the inherited TUI) the
  real terminal. Re-opening `/dev/tty` instead breaks Codex's crossterm event
  source (`reader source not set`); the dup is the fix. This whole dance exists
  **only because the wrapper backgrounds the process under a shell.**
- **`$TMUX_PANE`.** `grep` confirms the bridge, `codex_rpc`, and the TUI never
  read it. It is used in exactly one place: the wrapper's crash-record hook
  (`__orch-record-exit ... "$TMUX_PANE"`), which runs `tmux capture-pane` to
  attach an output tail to a post-mortem.

### Spike result

The harness spawns its PTY child as the **direct foreground process of a real
PTY** (`sh -c <cmd>`, PTY slave as controlling terminal), with `TMUX`,
`TMUX_PANE`, and `STY` removed, and **no stdin dup**. Codex booted fully under
exactly these conditions (49 KB TUI, alternate screen, alive). That is the same
process shape the session process will use, and it reproduces the bridge's own
inner shape: a parent that holds the terminal and spawns a null-stdio socket
child plus a stdio-inheriting child. So the unit runs in one Shelbi-owned PTY
without `$TMUX_PANE` and without duplicated stdin. Running the literal
`shelbi __codex-orchestrator` binary under the harness was not done because it
needs a fully built hub plus a Codex-runner project and would collide with the
live hub this spike runs under; the shape is proven, the binary swap is
mechanical.

### Fix plan for Phase 1 / Phase 2

1. **Spawn the bridge directly on the PTY slave.** The session process makes the
   bridge the PTY's foreground child, with fd 0/1/2 = the slave and the slave as
   controlling terminal. No backgrounding under a shell, so **delete the
   `exec 3<&0` / `<&3` dup**: it was only ever needed to rescue stdin for a
   backgrounded job.
2. **Move the heartbeat and signal traps out of the pane command.** They
   currently share the pane via backgrounded shell loops (`__zen-heartbeat`,
   the `__stop` traps). In the new model they become daemon/session supervision
   responsibilities (Phase 3 `rt-daemon-poller` / `rt-session-restarts-daemon`),
   which removes the second reason the dup existed.
3. **app-server is unchanged.** `Stdio::null()` plus a Unix socket is already
   PTY-agnostic and Windows-portable (a named pipe can stand in for the socket
   per the decision table).
4. **The remote TUI stays `Stdio::inherit()`.** It inherits the bridge's PTY
   slave, so it renders into the session's PTY with no change. The responder
   above is what lets it come up before any client attaches.
5. **Replace the `$TMUX_PANE` crash tail with a session snapshot.** The session
   owns the emulator, so the post-mortem output tail is a local snapshot call
   (the `snapshot` capability, Phase 1 `rt-snapshot`) instead of
   `tmux capture-pane`. Pass the session id where `$TMUX_PANE` is passed today.
6. **Keep the three-process unit intact.** Nothing here argues for collapsing
   bridge + app-server + TUI into one process. The point is only that the unit
   no longer needs tmux to host it.

No blockers. The Codex orchestrator is ready to move onto a session PTY behind
the backend seam (Phase 2 `rt-backend-sessions`), with the dup and the
`$TMUX_PANE` tail retired as described.
