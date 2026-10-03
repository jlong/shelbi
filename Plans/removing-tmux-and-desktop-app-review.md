# Review of "Removing tmux" and "Desktop App (gpui)"

Status: adversarial review of both plans as first written on 2026-10-02. Both plans have since been revised to incorporate it; see "Outcome" at the end.
Reviewed: 2026-10-02
Plans under review: [[removing-tmux]], [[desktop-app-gpui]]
Code baseline checked: Shelbi `7f331c5`

## Verdict

The direction holds. Nothing found argues for keeping tmux, and the split into
a PTY-owning host, a hub daemon, and thin clients is still the right shape for
a TUI and a desktop app to share.

The plans are wrong about cost and about several mechanisms.

**Removing tmux** describes what tmux does today accurately (nearly every code
citation checked out) but underestimates the replacement in three places:

1. The host is called "small" and is not. As designed it holds the emulator,
   a replay serializer that has to be written from scratch, and the terminal
   query responder, inside the one process that cannot restart without killing
   agents.
2. The terminal design did not account for how Claude Code and Codex actually
   behave: they query the terminal at startup, Claude Code's default mode
   takes over the screen and the mouse, and Shift+Enter depends on a keyboard
   protocol that is off by default in the chosen emulator.
3. Phase 4 (the single-process TUI) is the largest phase by a wide margin and
   the plan sizes it as a list of bullets. Phase 3 (poller to the daemon) is
   mechanically easy and semantically hard, the reverse of what the plan says.

**Desktop app** rests on two premises that are false in the code today: that
the CLI and TUI share their mutation logic, and that there is a command
registry to extract. Its gpui facts were written from memory and about half
were out of date. Its scope (full parity, four Linux channels, no preview) is
the part most likely to fail for a solo maintainer.

Recommendation in one line: keep both plans, revise the host design and the
phase list of the first before starting, and add two prerequisite pieces of
work to the second (move mutation logic into a library, and build the shared
app model during the TUI rewrite, not after it).

## How this was checked

- Three independent reviewers, none shown the others' work or my reasoning:
  one checked `removing-tmux` claim by claim against the code and hunted for
  tmux dependencies it missed; one attacked the host, emulator, protocol, and
  remote design using crate documentation and other multiplexers as prior
  art; one verified the desktop plan's external claims against current
  sources and tested the `shelbi-app` extraction against the TUI code.
- I spot-checked six of the most consequential code claims myself (the
  machines view, the CLI crate having no library target, the callers of
  `run_gated_merge`, the TUI's `move_card`, the tmux commands in shipped
  templates, the launchd `KeepAlive` setting). All six held.
- I did not re-verify the reviewers' web findings. Where a claim below rests
  on an external source it is cited, and where a reviewer could not confirm
  something it is marked.
- Nothing in the code checkout was modified. This document describes the
  plans as first written; the revisions are summarized under "Outcome".

## 1. Removing tmux: what the plan gets wrong

### 1.1 The host is not small

The plan's safety argument is that the host "should be small, change rarely"
because restarting it kills every agent. But the same plan puts in the host:
one emulator per session, attach replay, snapshots, title tracking, and (not
in the plan, but required, see 1.2) answering terminal queries. That is the
most intricate and bug-prone code in the design.

It gets worse because **attach replay does not exist to be used**.
`alacritty_terminal` has no way to serialize its state back into escape
sequences, and it keeps private several things a faithful replay needs: the
inactive screen (the primary screen while a full-screen program is running),
the keyboard-protocol stack, the scroll region, and tab stops. The plan
treats replay as given. Other tools solve reattach in one of three ways:
render per client from a server-side grid (tmux, zellij, wezterm's mux),
serialize from an emulator built for it (shpool, on a `vt100`-family crate),
or keep no screen state and make the program repaint by sending it a resize
signal (dtach, abduco).

Two further corrections to the "same emulator as Zed" argument: Zed pins its
own fork of alacritty at a git revision, not the published crate, and the
crate is 0.x with breaking minor releases.

Options, as a decision for you in section 5:

- **One host, hardened.** Keep the design, isolate each session so an
  emulator panic cannot take down the process, and accept that host bugs are
  expensive.
- **Holder plus restartable emulator.** The host owns only the PTY and a
  bounded ring of recent output bytes. Emulation, snapshots, and replay live
  in a process that can restart and rebuild from the ring. This is what
  actually makes the host small.

Either way, replay for full-screen programs should use the cheap method:
restore modes, then force a repaint with a resize signal. That covers Claude
Code in its default mode and Codex. A grid serializer is only needed for
primary-screen content (shells, agents in inline mode).

### 1.2 Nobody answers the terminal

Agents ask the terminal questions at startup: cursor position, device
attributes, foreground and background colors. `alacritty_terminal` computes
the answers but hands them to the embedder to write back to the PTY. Codex
exits if the cursor-position reply is late
(github.com/openai/codex/issues/2805, 4415). The plan assigns this to no one.
Consequences:

- The host must answer immediately, including with zero clients attached, and
  including color queries it has no real answer for. Clients should report
  their colors at connect so the host has something to say.
- Every client emulator fed the same bytes will also produce answers; those
  must be discarded.
- In `shelbi attach`, the user's real terminal answers too, and its replies
  arrive on stdin and get typed into the agent. So attach needs a parser to
  strip queries from what it forwards, which contradicts the plan's "needs no
  emulator". It also needs a cleanup sequence on detach or crash (pop the
  keyboard protocol, disable mouse and paste modes, leave the alternate
  screen, show the cursor), or it leaves the user's terminal broken. The plan
  has none.

### 1.3 Claude Code's default mode conflicts with the scrollback design

Per Claude Code's documentation (code.claude.com/docs/en/fullscreen),
full-screen rendering on the alternate screen with mouse capture is the
default for users who started on or after 2026-05-06. In that mode the agent
owns scrolling, selection, and clicks, and the alternate screen has no
scrollback at all.

So three of the four "must have" terminal features (scrollback, selection,
search) are either dead for those sessions or would steal the agent's mouse.
The plan's "wheel enters scrollback, drag selects" has to become: when the
program has mouse reporting on, forward mouse events to it, and use
Shift+drag and Shift+wheel for Shelbi's own selection and scrolling. Shelbi's
scrollback and search then apply to primary-screen sessions only. Whether to
force agents into inline mode instead is a decision for you (section 5).

Related: the kitty keyboard protocol is off by default in
`alacritty_terminal`, and Claude Code's Shift+Enter depends on it. It must be
switched on.

### 1.4 Environment and shell assumptions

- **`$SHELL -l -c` skips the interactive rc file.** Today the launch command
  is pasted into an interactive login shell. `zsh -l -c` reads `.zprofile`
  but not `.zshrc`, where nvm, fnm, and Homebrew PATH setup usually live.
  Agents would not be found. The same applies to
  `ssh host shelbi host proxy` and to probing the remote's PATH. Use an
  interactive login shell, or capture that environment once and reuse it.
- **The host freezes the environment of whoever started it.** `TMUX`,
  `TERM_PROGRAM`, `SSH_AUTH_SOCK`, and the rest would leak into every agent
  for the host's lifetime. Spawn must take an explicit environment from the
  requesting client; the host should scrub terminal-identity variables and
  set `TERM=xterm-256color` and `COLORTERM=truecolor` (no custom terminfo, so
  nothing to install on remotes).
- **The daemon's environment is not the user's either.** The installed unit
  bakes a minimal PATH and already needs healing to find `gh`
  (`supervise.rs:167-200`). Once the poller lives there it runs git pushes,
  rebases, workflow actions, and SSH from that environment.

### 1.5 Sizing, protocol, and remotes

- **Sizing.** "Smallest attached viewer" means a forgotten small
  `shelbi attach` shrinks everyone, and each resize makes the agent repaint.
  Size to the most recently active client and debounce. (The plan's claim
  that smallest is tmux's default could not be confirmed and may be wrong.)
- **Client emulators must be locked to the PTY size**, with resizes delivered
  as sequenced frames inside the output stream. Otherwise host and client
  reflow differently and diverge. Output frames need sequence numbers anyway,
  for reconnect.
- **No backpressure.** A stalled client either blocks the PTY reader, which
  stalls the agent, or grows a queue without bound. Use bounded per-client
  buffers and, on overflow, drop and resend a snapshot. Also define what
  happens when two clients type at once.
- **sshd caps multiplexed sessions at 10 per connection by default.** A proxy
  per session plus the poller's SSH calls can exceed it. Use one long-lived
  proxy channel per hub that carries all sessions, with keepalives.
- **A remote host may not survive logout on Linux.** systemd-logind's
  `KillUserProcesses` kills the session's processes; its manual says this is
  what breaks tmux and screen unless they move out of the session scope.
  Start the host with `systemd-run --user --scope` where available, with
  lingering enabled.
- **Input encoder.** Use termwiz's key encoder (MIT) instead of writing one.
  The Phase 0 fallback of forwarding raw stdin bytes is not available:
  crossterm owns stdin, and mouse coordinates need translating regardless.
- **Memory.** 10,000 lines at 200 columns is on the order of tens of
  megabytes per full session by the reviewer's estimate, duplicated in the
  host and in every client. Clients should hold scrollback only for sessions
  being viewed.
- **The raw disk log** is repaint noise for full-screen agents: large,
  unreadable, and containing whatever was pasted. A text snapshot at exit
  plus a bounded ring serves post-mortems better. You chose the raw log; it
  is listed for reconsideration in section 5.

### 1.6 False statements about the code

Most citations held. These did not:

| Plan says | Actually |
| --- | --- |
| Tasks, machines, and activity views "already exist as ratatui code" | The machines view is a shell loop that runs `shelbi workspace list` every 5 seconds (`shelbi-orchestrator/src/lib.rs:1758`). It must be written. |
| Mode-aware `paste` replaces guesswork | `paste-buffer -p` already brackets only when the program asked. The real fragility is the Enter-after-paste race, which the host does not remove. `send_verified` stays for that reason. |
| tmux is referenced from three crates | True of Cargo dependencies only. tmux-specific code also lives in `shelbi-state` (`to_tmux_key`, `tmux_palette_key`, three instruction templates), `shelbi-core` (`TmuxAddr`), and `shelbi-agent` (launch strings). |
| Remove `spawn`, `archive`, `tail`, and "the pane paths in `merge`" | `shelbi attach` already exists as a legacy command, so the new command collides with it. All of `merge.rs` is built on the legacy agent record. `list.rs`, `diff.rs`, `send.rs`, and `app.rs:1935` also read it. |
| About 70 `tmux_available()` guards | 47. |
| Poller is 11,462 lines | 5,390 of code and 6,072 of tests. |

### 1.7 What the plan omits

- **The poller drives UI layout.** It calls `ensure_dashboard` to relaunch a
  crashed orchestrator and several `review_ui` functions that build and tear
  down panes. A daemon cannot do layout. These must be split into session
  operations (daemon) and layout (client) before the move.
- **"A project is open when its orchestrator session exists" breaks
  orchestrator supervision.** Today the tmux session outlives a dead
  orchestrator pane, which is what lets supervision restart it. Openness
  needs its own record. The daemon, which is hub-global, also needs a
  per-project poller manager.
- **On-demand start fights the installed unit.** The launchd plist sets
  `KeepAlive` with a one-second throttle, and the daemon's lock fails fast.
  If the CLI starts a daemon first, launchd respawns a failing copy every
  second. `shelbi daemon restart` and the version-mismatch flow assume a
  supervisor exists. Pick one owner of the daemon's lifecycle.
- **`shelbi reload` is never mentioned.** It respawns panes in place so a new
  binary takes effect. It needs redefining for clients, daemon, orchestrator
  session, and a host that must not restart.
- **Shipped agent instructions tell agents to run tmux**
  (`default_orchestrator.md.template:588`, `default_review.md.template:85`, a
  skill file). Review events carry a tmux target. `reload` preserves
  user-edited instructions, so existing projects keep the stale text unless
  a config-upgrade rule rewrites it.
- **The Codex orchestrator is three processes in one pane** (app-server,
  bridge, remote TUI) and depends on duplicated pane stdin and `$TMUX_PANE`.
  The spike covers only plain agents in a PTY.
- **The overlays are not in `shelbi-tui`.** Palette, review confirm, reject
  reason, error log, and zen intro are separate `shelbi-cli` processes with
  their own event loops, about 5,000 lines, returning results through temp
  files. "Overlays" in Phase 4 is a port of all of that.
- **Single process means one blocked call freezes everything.** There are
  four independent event loops today, and several synchronous calls
  (`focus_workspace`, opening the review interface, approving a review,
  cutting a branch on move) that currently freeze one pane. In one process
  they freeze every terminal.
- **Per-client state.** "Persist current view to `state.json`" has any number
  of clients overwriting one cross-project file.
- **Windows-readiness covers only the host socket.** Workers reach `hub.sock`
  from shell hooks using `nc -U`, socat, or python; 33 files use Unix-only
  APIs. The claim should be narrowed to "the new code does not add to the
  problem".
- **The website in the code repo** has about 100 tmux mentions, plus
  `AGENTS.md` and the Homebrew tap doc.

### 1.8 The session trait is too small

The proposed trait has nine operations. The orchestrator also needs: a
three-state probe with a deadline (dead, alive, unreachable, where
unreachable must be an error and not "dead"), respawn in place, enumeration,
retention of a dead session's screen, resize, and a per-target injection
lock. `load.rs`, `issue.rs`, `send.rs`, `open.rs`, `open/pane.rs`, and
`wake.rs` are missing from the Phase 2 file list.

One clarification that helps: of roughly 280 tmux touch points, about 78 are
session operations through `shelbi-tmux` and about 200 are raw layout calls.
The trait should cover only the first group. The layout calls are not ported
behind any abstraction; they are deleted when the single-process TUI replaces
them. `snapshot` must also reproduce the text shape of `capture-pane -p -J`,
because about 80 detector and baseline tests are anchored on it.

### 1.9 Sizes

| Phase | Size | Why |
| --- | --- | --- |
| 0 Spikes | Small to medium | Add the Codex orchestrator unit, full-screen Claude Code, and the query responder to the spike list. |
| 1 Host, protocol, client, attach | Large | Four new crates, replay, query handling, input encoding. No existing code to reuse. |
| 2 Session trait | Large | About 78 uses plus about 40 raw calls in session-level files; about 300 tests there, about 50 against real tmux. |
| 3 Poller to daemon | Medium to move, large to make correct | The file has no dependencies on the TUI and does not need a new crate. The work is lifecycle, environment, the unit conflict, and pulling UI calls out. |
| 4 Single-process TUI | Very large | Merge four event loops, port 5,000 lines of popup processes, rewrite `review_ui.rs` and the dashboard half of the orchestrator's `lib.rs`, write the machines view, move blocking calls off the UI thread. |
| 5 Remote hosts | Medium to large | New install and version path; replaces the paste launch and the proxy window. |
| 6 Cutover | Large | Delete about 200 raw call sites, rewrite templates with an upgrade rule, website docs, packaging. |

## 2. Desktop app: what the plan gets wrong

### 2.1 The CLI and the TUI do not share mutation logic

`shelbi-cli` is a binary with no library target. The real move logic,
including the gated merge and transition actions, lives inside it
(`commands/issue.rs:851-1031`), as does dispatch (`issue.rs:1332-1737`). The
TUI's own `move_card` (`shelbi-tui/src/kanban.rs:1485`) cuts a branch and
changes status; `run_gated_merge` has exactly two callers, the CLI and the
review interface.

So "writes go through the same code paths the CLI uses" is false, and a
desktop app could not call that logic without shelling out to `shelbi`. It
also suggests that moving a card to done on the TUI board today may skip
what `shelbi issue move` does. That is worth checking as a present-day bug,
independent of either plan.

Prerequisite work: move `move_to`, `start`, `assign`, `edit`, and `add` out
of the CLI into a library crate, and make the TUI call them.

### 2.2 There is no command registry, and far less model code than claimed

Palette dispatch is string-prefix matching inside a 2,916-line ratatui file
in the CLI binary, which the plan's file list does not even include. The only
action enum is mostly cursor motions. Phase 1 "extract `shelbi-app`" is a
design and build.

The "about 32,000 lines" figure overstates what is reusable by roughly five
times: after removing the poller and tests, about 11,000 lines remain, of
which the reviewer estimates 6,000 to 7,000 are model and control. That code
stores ratatui `Rect` hit areas, computes layout in cells, and makes 27 tmux
calls. `shelbi-state` itself depends on crossterm for key chords.

The consequence for sequencing: extracting after the TUI rewrite means
writing the TUI's model twice. `shelbi-app` should be written during the
tmux plan's Phase 4, with "no ratatui or crossterm types in `shelbi-app` or
`shelbi-state`" as an exit criterion there. The tmux plan currently says only
"where it is cheap to do so".

### 2.3 Refresh blocks, and there is no file watching

The TUI polls on a 200 ms event tick and a 750 ms refresh. That refresh runs
synchronous SSH (`cat` over SSH per review task on remote machines), a daemon
version probe, and git operations. On gpui's foreground thread this freezes
the window. The plan's `notify`-based watching does not exist today and would
miss GitHub-backed boards, which change through the daemon's index, not task
files. All refresh and commands in `shelbi-app` must run off the UI thread
and publish snapshots; the daemon should push change events.

### 2.4 The parity test is trivially satisfiable

"Every command has a binding or menu path" passes with a menu item wired to
nothing. A real check is: an exhaustive match per front end so a new command
fails to compile until handled, headless scenario tests on `shelbi-app`
asserting model and event effects, and a smoke test per front end that
invokes each command through its UI path.

### 2.5 gpui facts

| Plan says | Found |
| --- | --- |
| "Pin a gpui version" | There is no clean channel. crates.io `gpui` is 0.2.2 from October 2025. Zed's main has split it into unpublished platform crates. The live options are a git dependency on the Zed repository (which means `shelbi-desktop` cannot be published to crates.io) or `gpui-pre`, weekly snapshots published by one engineer at Longbridge. |
| (not addressed) | Zed pins a much newer Rust than Shelbi's `rust-version = "1.88"`, and CI runs `cargo check --workspace`, which ignores `default-members`. The desktop crate needs its own `rust-version`, exclusion from the main CI job, and its own job. |
| Terminal element "cannot be borrowed" | The GPL claim is correct. But `gpui-terminal` (MIT or Apache-2.0) exists as a seed to study, on older gpui. |
| Linux renders with Vulkan | Now wgpu with Vulkan and GL backends. |
| Hyprland is a test target | Zed's Linux documentation names Omarchy for a GPU driver launch failure, and a Hyprland "no window appears" issue was closed as not planned. Omarchy is a named risk, not just a target. gpui apps also draw their own window decorations on GNOME. |
| Accessibility is limited | gpui now integrates AccessKit. A custom-painted terminal still exposes nothing unless Shelbi builds the tree. |
| Notifications, implied extra work | gpui main has system notifications built in. |
| `gpui-component` | Now `gpui-kit`, Apache-2.0, weekly releases, pinned to `gpui-pre`. Adopting it decides the gpui channel. No kanban or terminal component found. |
| Homebrew casks may be macOS-only | Casks now work on Linux. One cask can cover both platforms; no separate Linux formula needed. |
| A build runner per target | A binary built on `ubuntu-latest` will not run on older distributions. Zed builds on an old-glibc base and bundles libraries. Intel macOS can be cross-built from Apple silicon, and Homebrew has moved Intel macOS to its lowest support tier. |
| App finds `shelbi` at the Homebrew prefix, then PATH | Apps launched from Finder do not get the shell's PATH, which also affects the `git`, `gh`, and `ssh` the orchestrator spawns. Resolve the login-shell environment and offer a settings override. |

Not verified by the reviewer: AppImage tooling for a GPU app, exact AUR
`-bin` requirements, whether Linux gets native menus, and how Omarchy users
would actually install it. On the last point, getting into Omarchy's own
package repository may matter more than the AUR.

### 2.6 Scope

The parts most likely to break the plan for one maintainer, in order: the
terminal element with IME, selection, and links across three compositor
classes; the unbudgeted prerequisites in 2.1 and 2.2; review-flow parity
(the TUI spawns editor and diff through tmux and popup processes); seven
install-verification jobs across the channels; keeping up with gpui (weekly
if on `gpui-pre`); and GPU bug reports with no hardware lab.

## 3. What the plans get right

- The inventory of what tmux does today, and nearly every code citation.
- The host and daemon as separate processes, and the same host binary on the
  hub and on remotes.
- Leaving the `hub.sock` event protocol and the plain-file state model alone.
- State back-compat: `TmuxAddr` is persisted only in legacy agent records,
  workspace addresses are computed, and unknown fields are preserved, so old
  files load.
- `shelbi:<state>` titles are set by OSC 2 from hook scripts with no tmux
  command involved, so that signal carries over unchanged.
- `alacritty_terminal` exposes damage, grid, mode flags (including bracketed
  paste and mouse modes), resize, search, and title and exit events.
  `portable-pty` covers Unix and ConPTY.
- gpui is Apache-2.0; Zed's terminal crates are GPL; a cask can depend on a
  formula; `shelbi-desktop` and `shelbi` do not clash; the macOS floor is far
  inside gpui's range.
- Hard cut, not a long-lived dual backend.

## 4. Recommended shape

**Removing tmux**

1. **Phase 0 grows.** Spike full-screen Claude Code (mouse and scroll
   ownership), the terminal query responder with zero clients, the Codex
   orchestrator unit, replay by forced repaint, and the emulator choice
   (vendored alacritty fork against a `vt100`-family crate that can
   serialize).
2. **Decide the host shape before Phase 1** (section 5, first decision).
3. **Phase 2 stays, with a wider trait** covering session operations only.
   Layout calls are not abstracted; they are deleted in Phase 4.
4. **Phase 3 gets a design pass first:** daemon lifecycle ownership,
   environment, per-project poller manager, the definition of an open
   project, and splitting the poller's layout calls out. No new crate
   needed.
5. **Split Phase 4** into at least: mutation logic into a library (shared
   with the desktop plan); `shelbi-app` model and command registry; terminal
   view; native views including a new machines view; overlays port; review
   interface; blocking calls off the UI thread.
6. **Add to cutover:** `shelbi reload` semantics, instruction templates plus
   an upgrade rule, the `attach` name collision, the full legacy list
   including `merge`, per-client UI state, the website.

**Desktop app**

1. Fold "Phase 1: extract `shelbi-app`" into the tmux plan's Phase 4.
2. Decide the gpui channel in the spike, and set up the separate toolchain
   and CI job on day one.
3. Start the terminal element from `gpui-terminal` as a reference.
4. Replace the parity test.
5. Narrow the first release (section 5).

## 5. Decisions only you can make

1. **Host shape.** One hardened host with the emulator inside, or a minimal
   holder process with emulation in a restartable process. The second is more
   moving parts and is the only version where "the host rarely changes" is
   true. My lean: the holder, because an emulator bug should never cost you
   every running agent.
2. **Sizing rule.** Most recently active client, debounced, replacing
   smallest-viewer. My lean: yes.
3. **Full-screen agents.** Leave Claude Code in its default full-screen mode
   and forward the mouse, which makes Shelbi's own scrollback, selection, and
   search apply only to primary-screen sessions; or force inline mode so
   Shelbi owns those features everywhere. My lean: leave the default and
   forward. Fighting the agent's own UI recreates the kind of friction this
   project is meant to remove. This reduces the weight of three of your four
   terminal must-haves.
4. **Disk log.** You chose raw output on by default. Given 1.5, would you
   take a text snapshot at exit plus a bounded ring, with the raw log
   opt-in?
5. **Daemon lifecycle.** Started on demand by the CLI, or owned by the
   launchd and systemd unit. It cannot be both without the conflict in 1.7.
6. **gpui channel.** `gpui-pre` with `gpui-kit` (fast, weekly churn, depends
   on one outside maintainer), or a git dependency on Zed (authoritative, no
   widget library, not publishable to crates.io).
7. **First desktop release scope.** You chose four Linux channels and no
   preview. The review's advice is macOS plus one Linux channel, and to
   reconsider a preview, given the terminal element and Linux GPU risk. Also
   whether to ship Intel macOS at all.
8. **The possible present-day bug in 2.1.** Whether a TUI board move to done
   is meant to skip the gated merge.

## Appendix: reviewer confidence

Claims marked by the reviewers as verified against a source: the
`alacritty_terminal` API gaps, the Codex cursor-position issues, Claude
Code's full-screen default, sshd `MaxSessions`, logind `KillUserProcesses`,
Zed's alacritty fork, the gpui crates.io state, Zed's terminal crate
license, Homebrew casks on Linux, and all code citations.

Marked likely, not verified: per-client emulator divergence, environment
inheritance effects, the memory estimate, backpressure behavior, Finder PATH
behavior.

Could not verify: tmux's default window-size rule, AppImage tooling, AUR
requirements, Linux native menus, Omarchy's install path.

## Outcome

Decided by John on 2026-10-02 and folded into both plans.

| Decision | Outcome |
| --- | --- |
| 1. Host shape | Neither option offered. One small process per session, with the emulator beside its PTY and no shared host, chosen as the most reliable shape: a bug costs one agent at most and upgrades never touch running agents. |
| 2. Sizing | Most recently active client, debounced. |
| 3. Full-screen agents | Left in their default mode; the mouse is forwarded; Shift+wheel and Shift+drag are Shelbi's. |
| 4. Disk log | Text snapshot at exit; raw log opt-in per project. |
| 5. Daemon lifecycle | On demand only. The launchd and systemd units are retired. |
| 6. gpui channel | Decided in the spike, defaulting to `gpui-pre` with `gpui-kit` if both work. |
| 7. First desktop release | macOS on Apple silicon only, still held until full parity with no preview. Linux desktop is a later release. |
| 8. Board move skipping the gated merge | A bug. Confirmed in the code (`kanban.rs:1485` against `issue.rs:908`). To be fixed now as its own task, ahead of both plans. |

Everything in sections 1 and 2 was incorporated, with these judgment calls
made in the revision:

- A project is "open" by a record in its `state.json`, not by a session
  existing.
- `shelbi reload` restarts the daemon, re-execs attached TUI clients, and
  hands off the orchestrator; worker sessions are untouched.
- At cutover, a project with a surviving `shelbi-*` tmux session refuses to
  open until that session is closed, so two pollers never run together.
- The shared `shelbi-app` model moved from the desktop plan into the tmux
  plan's Phase 4a.
- The Windows claim was narrowed to the new session code.

## Second review: Codex, 2026-10-02

Codex reviewed the revised plans against `7f331c5` and reported eight gaps.
I checked every code citation; all held. All eight were accepted.

| # | Finding | Change made |
| --- | --- | --- |
| 1 | Long-lived processes break cancellation: `issue start` abandons a timed-out thread expecting the CLI to exit (`issue.rs:1536`), and the poller leaves stuck threads for the OS to reap (`poller.rs:346`). | Generations on every daemon job, deadlines on subprocesses, and a quit barrier. Tests in the tmux plan's Phase 3. |
| 2 | Shared mutation functions do not make concurrent clients safe; `move_to` reads, merges, then writes with no lock across the operation (`issue.rs:858`). | John decided the daemon executes all mutations: one per issue at a time, with expected state and a recheck before irreversible steps. |
| 3 | Cutover can leave old remote agents running beside new ones; teardown reports a failed remote kill as done (`teardown.rs:376`). | Migration is tracked per workspace. Remote shutdown must be verified; an unreachable remote stays pending and cannot be dispatched to. |
| 4 | A resize signal does not reliably repaint, and never rebuilds the normal screen under a full-screen program. | Replay now serializes full emulator state for both buffers. The forced repaint is at most a nudge. This raises the cost of Phase 1 and sharpens the emulator choice. |
| 5 | With the service units retired, nothing restarts a daemon that crashes while no UI is attached. | Session processes watch the daemon's lock and restart it when their project is open. |
| 6 | A versioned hello detects incompatibility but does not solve it; the desktop app keeps running its old version after `brew upgrade`, and the existing guard rejects any version difference (`hub_version.rs:177`). | Two policies: a frozen core protocol for sessions, exact match for daemon and apps with a relaunch prompt in the desktop app. |
| 7 | Passthrough attach cannot honor the sizing rule; raw bytes for a wide session wrap wrongly in a narrow terminal. | `shelbi attach` now renders through `shelbi-term`. This also removes the query double-answer and terminal-restore problems. |
| 8 | Excluding the desktop crate from `cargo check` misses the other workspace-wide commands (`app-ci.yml:54-68`, `release.yml:71`), and a shared lockfile can break the CLI's Rust floor. | Went further than suggested: `shelbi-desktop` is a separate Cargo workspace in the same repository, with its own lockfile and toolchain. |

The two changes with the largest effect on scope are 2 (the daemon becomes a
command server every client depends on) and 4 (the replay serializer must
cover everything, not just the normal screen).
