# `rt-spike-runtime` findings

Phase 0 spike items **5 (process survival)** and **6 (nesting)**, plus the
`portable-pty` checks the plan lists under "PTY". Prototype lives in
[`spikes/remove-tmux/runtime/`](../../../spikes/remove-tmux/runtime) (outside the
cargo workspace). Everything below was produced by that prototype; reproduce
with:

```sh
cargo test --manifest-path spikes/remove-tmux/runtime/Cargo.toml
# nesting observations are also written to the spike's
# target/nesting-findings.txt and printed with `-- --nocapture`
```

Host used: macOS (Darwin 25.6.0, arm64), `portable-pty` 0.9.0, tmux 3.7b, GNU
Screen 4.00.03 (the copy Apple ships). Linux was not reachable this session;
see the note under Process survival.

## Summary

| Item | Result | Fixable? |
| --- | --- | --- |
| Survive launcher exit (macOS) | Works (setsid + stdio to `/dev/null`) | n/a |
| Survive over `ssh host ...` | Works (stdio redirect, launcher returns at once) | n/a |
| Survive launcher exit (Linux/logind) | **Untested** (no reachable Linux host) | recipe below |
| Child process group dies on kill | Works (`killpg` reaps child + grandchild) | n/a |
| `portable-pty` controlling terminal + own session | Works | n/a |
| `portable-pty` group kill reaches whole group | Works | n/a |
| `portable-pty` no descriptor leak into child | Works (child holds only fds 0/1/2) | n/a |
| kitty keyboard inside tmux / Screen | **Not available** | partial (see below) |
| truecolor inside tmux / Screen | Falls back to 256 unless configured | yes (tmux); no (this Screen) |
| OSC 52 copy inside tmux / Screen | Not forwarded to the outer terminal | config-dependent (tmux); no (this Screen) |

## Process survival (spike item 5)

### macOS: works

The launcher (`detach-spawn`) spawns the session with `setsid()` in a
`pre_exec` hook and all three stdio fds pointed at `/dev/null`, then returns
immediately without waiting. macOS has no `setsid(1)` binary, so the detach is
the `setsid(2)` syscall, which is what matters.

After the launcher exits, the session is reparented to init and is a session
leader with no controlling terminal:

```
  PID  PPID  PGID STAT TPGID COMMAND
  258     1   258 Ss       0 rt-runtime session ...
```

`PPID 1` (launcher gone), `STAT Ss` (session leader), `TPGID 0` (no controlling
terminal). The session keeps advancing its marker file after the launcher is
gone, which is the real liveness signal the test asserts on. Verified by
`survival::survives_launcher_exit_and_group_kill`.

### Over `ssh host ...`: works, by the same redirect

The property that keeps a launching `ssh` from hanging is that the session does
not hold the launcher's stdio. `ssh` returns when its channel's fds reach EOF;
if the long-lived session inherited them it would keep them open and `ssh`
would block until the session died.

The spike tests this directly without needing a second host: the launcher's own
stdout is a pipe, and because the session redirects its stdio to `/dev/null`,
the pipe reaches EOF the instant the launcher exits, while the session runs on.
Verified by `survival::stdio_redirected_so_launcher_pipe_closes`. The same
redirect is what makes the remote spawn in Phase 5 safe.

### Child process group dies on kill: works

The session spawns its child through `portable-pty`, which puts the child in a
new session (so the child leads its own process group). The child in turn starts
a grandchild in that same group. On `SIGTERM` the session calls
`killpg(child_pgid, ...)`, and the whole group dies:

```
### child process group 260 members:    260 sleep 100000 / 261 sleep 100000
### after SIGTERM, group survivors:      (none)
```

Verified by `survival::survives_launcher_exit_and_group_kill` and, against
`portable-pty` directly, by `pty::group_kill_reaps_grandchild`.

### Linux with logind: untested this session

The project's Linux machine (`devbox` in `~/.shelbi/projects/shelbi.yaml`) is a
Tailscale SSH host. Reaching it needs an interactive browser login
(`https://login.tailscale.com/a/...`) that this automated session cannot
complete, so the Linux path is **not verified here**. To verify it, authenticate
Tailscale (`! ssh devbox true` from an interactive Shelbi prompt) and run the
survival test on that host.

The recipe to verify there (and the one the production spawn should use) is in
the next section. The specific logind risk to confirm: with
`KillUserProcesses=yes` (the logind default on many distros), `setsid` alone is
not enough. The session is killed at logout unless it is in a scope that
survives, which is why the plan prefers `systemd-run --user --scope` with user
lingering enabled.

## `portable-pty` behavior (plan item "PTY")

All verified against `portable-pty` 0.9.0 in [`tests/pty.rs`](../../../spikes/remove-tmux/runtime/tests/pty.rs).

- **Controlling terminal and process group.** The child's `tty` resolves to a
  real device, and read from the parent its session id and process-group id both
  equal its own pid, and its group is the foreground group of the pty. So
  `portable-pty` does the `setsid()` + `TIOCSCTTY` dance for us; the session
  process does not have to. (`child_has_controlling_tty_and_own_session`.)
- **Group kill reaches the whole group.** Covered above.
- **No descriptor leak into children.** The child holds exactly fds 0, 1, and 2,
  all pointing at the pty slave (a character device). The pty master, which is a
  character device too, does not appear, so nothing leaks in. The test counts
  char-device fds rather than comparing fd numbers, which are not comparable
  across processes. (`no_descriptor_leak_into_child`.)

Implication for Phase 1: the session process can rely on `portable-pty` for
controlling-terminal setup and for keeping its own fds (socket, lock, emulator
pipes) out of the child, provided those fds are `CLOEXEC`, which Rust sets by
default.

## Nesting (spike item 6)

### Method and why the baseline matters

The production TUI is an ordinary full-screen program, so nesting runs by
construction; the open question is which terminal capabilities survive the outer
multiplexer. The spike runs a small `probe` stand-in inside tmux and inside
Screen, with the test itself emulating the **outer** terminal on a real PTY: it
answers the kitty-keyboard query as a kitty-capable terminal would, answers the
primary-device-attributes (DA1) query, and watches the outer byte stream for the
app's OSC 52 write.

A no-multiplexer **baseline** validates that emulator end to end (kitty query
answered, OSC 52 seen). The baseline passes, so a `no`/`false` under a
multiplexer is a genuine passthrough gap, not a harness artifact. See
[`tests/nesting.rs`](../../../spikes/remove-tmux/runtime/tests/nesting.rs).

### kitty keyboard protocol: not available inside tmux or Screen

| Context | App's `CSI ? u` query round-trips? |
| --- | --- |
| baseline (no multiplexer) | yes |
| tmux, `extended-keys off` (default) | no |
| tmux, `extended-keys on` | no |
| GNU Screen 4.00.03 | no |

The query never reaches the emulated outer terminal through either multiplexer,
so the app sees no reply and must conclude the protocol is unavailable. This is
exactly the plan's concern: Claude Code's Shift+Enter rides on the kitty
protocol, and crossterm requests the kitty
form rather than the older modifyOtherKeys form.

Turning tmux's `extended-keys on` did **not** make the kitty query round-trip.
That setting is still the right one-line mitigation to recommend, because it is
what lets tmux carry modified keys at all (in its CSI-u / modifyOtherKeys
encoding) between the outer terminal and the pane:

```tmux
set -s extended-keys on
# and tell tmux the outer terminal accepts them:
set -as terminal-features ',*:extkeys'
```

But it is a mitigation, not a fix: tmux does not transparently pass the kitty
protocol through, so an app keyed off the kitty handshake will not light up just
because `extended-keys` is on. **Fixability: partial.** The robust answer is the
one the plan already lands on: detect the missing round-trip and tell the user
once, with the `extended-keys` line, rather than assume Shift+Enter works.
Inside Screen 4.00.03 there is no setting that helps.

### truecolor: inherited in the environment, but renders as 256 unless configured

`COLORTERM=truecolor` reached the pane under both tmux and Screen (it is
inherited from the environment the multiplexer started in). That is misleading:
the env var surviving does not mean 24-bit color survives. tmux emits 24-bit to
the outer terminal only when its `terminal-features` include `RGB` for that
terminal, which the default set here does not, so it quantizes to 256. Screen
4.00.03 has no truecolor and quantizes as well. `TERM` as seen by the pane:
`xterm-256color` under tmux (we set `default-terminal`), `screen.xterm-256color`
under Screen.

**Fixability:** yes for tmux, with one line per terminal:

```tmux
set -as terminal-features ',xterm-256color:RGB'
```

For this Screen, no; treat truecolor as unavailable and fall back to 256, which
the client emulator will do anyway.

### OSC 52 copy: not forwarded to the outer terminal

The app's OSC 52 clipboard write was **not** observed reaching the outer
terminal under tmux (default `set-clipboard external`) or under Screen, even
with the probe lingering half a second after emitting so a lazy flush would have
been caught. Under tmux the write lands in tmux's own paste buffer rather than
being relayed to the outer terminal's clipboard; whether it is relayed depends
on `set-clipboard` and on the outer terminal advertising the clipboard
capability. Screen 4.00.03 has no OSC 52 support. This matches the plan's note
that OSC 52 copy depends on the outer multiplexer's clipboard setting.

**Fixability:** config-dependent for tmux (`set-clipboard on` plus an outer
terminal that accepts OSC 52); not available for this Screen. For Shelbi this is
a "copy may land in tmux's buffer, not the OS clipboard, when nested" caveat to
surface, not a blocker.

### Caveat on the nesting harness

The emulated outer terminal answers only the kitty and DA1 queries, not the full
set a real terminal replies to, and the Screen tested is Apple's ancient
4.00.03. The kitty-keyboard result is the firm one (explicit handshake,
baseline-validated). The truecolor and OSC 52 results are best read together
with the mechanism notes above, which is how they are reported.

## Recommended spawn recipe per platform

For `shelbi __session` in Phase 1. The environment handling (capture the login
shell's env once, scrub `TMUX`/`TMUX_PANE`/`TERM_PROGRAM`/`STY`, set
`TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=shelbi`) is the same
on every platform and is unchanged from the plan.

- **macOS:** `setsid(2)` via a `pre_exec` hook (there is no `setsid(1)`), all
  stdio to `/dev/null`, do not wait on the child. Verified here.
- **Linux with logind:** prefer `systemd-run --user --scope` with user lingering
  enabled (`loginctl enable-linger $USER`), so `KillUserProcesses=yes` does not
  reap the session at logout. Fall back to `setsid(2)` where `systemd-run` is
  unavailable. Stdio to `/dev/null`. Not verified this session; verify on the
  Linux host as above.
- **Over SSH:** no different from the local recipe. The stdio redirect is what
  lets the launching `ssh` return; the detach is what lets the session outlive
  the connection. Verified here via the redirect property.

In all cases the child the session runs is spawned through `portable-pty`, which
gives it its own session and controlling terminal and keeps the session's own
fds out of it.

## Exit-criteria list: what does not work, and whether fixable

1. **kitty keyboard protocol inside tmux (any `extended-keys`) or Screen.** Does
   not work. Partially fixable (`set -s extended-keys on` carries modified keys
   but does not restore the kitty handshake); the real handling is to detect and
   notify once.
2. **Truecolor inside tmux without an `RGB` terminal-feature, and inside Screen
   4.00.03.** Renders as 256. Fixable on tmux with one `terminal-features` line;
   not on this Screen. Fall back to 256 regardless.
3. **OSC 52 copy out to the OS clipboard when nested.** Not forwarded by default
   tmux or by this Screen. Config-dependent on tmux; unavailable on this Screen.
   Surface as a caveat.
4. **Linux/logind survival.** Not a failure, but untested this session because
   the only Linux host needs interactive Tailscale auth. Recipe documented;
   verify before the cutover packaging task adds its CI smoke job.

Everything else tested (macOS survival, SSH-safe detach, group kill, and all
three `portable-pty` behaviors) works as the plan assumes.
