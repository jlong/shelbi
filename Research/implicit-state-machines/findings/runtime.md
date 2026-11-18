# Implicit state machines: runtime slice

**Slice:** `crates/shelbi-session`, `crates/shelbi-ssh`, `crates/shelbi-term`, `crates/shelbi-agent`, `crates/shelbi-palette`, and `scripts/` (surveyed 2026-11-18 against `origin/main` e8145b3f).
**Findings:** 12 total: **2 high**, **5 medium**, **5 low**, plus a Minor list. The two high-risk machines are the session host's on-disk lifecycle (one process, ~8 marker files, state re-derived by four predicates in `shelbi-client`) and the per-host SSH reverse-forward/ControlMaster health machine (persisted mode record, process-global statics, implicit master opens).
**Coverage:** every file was read or grepped and the candidates read in full: shelbi-session (all 15 src files + 4 test files), shelbi-ssh (`lib.rs`, `askpass.rs`, `tests/forward_recheck.rs`), shelbi-term (all 12 src files + golden test), shelbi-agent `lib.rs`, shelbi-palette `lib.rs`, and all 11 files under `scripts/`. No findings in: shelbi-agent (pure command builders: `launch_command`, `with_*` flag injection, `RunnerAdapter` capability lookups; agent *process* lifecycle lives in the orchestrator, see leads), shelbi-palette (pure fuzzy matcher), `session/vt.rs` + `output_split.rs` (already an explicit `State` enum with a `step` fn, cross-checked against `vte` by a fuzz test: a healthy model), `replay.rs`, `emulator.rs`, `history.rs`, `procinfo.rs`, `layout.rs` (data/pure), shelbi-term `selection.rs`, `links.rs`, `render.rs`, `viewport.rs`, `lib.rs`.

## High

### 1. Session host lifecycle (on-disk session directory)

- **Entity:** one `shelbi __session` process and its directory `~/.shelbi/sessions/<short-id>/`.
- **Encoding:** 3 (on-disk markers: dir, `lock` flock, `sock`, `meta.json`, `exiting`, `final.txt`, `exit.json`, plus the client-written `kill-reason`), 4 (process liveness via flock, socket reachability via a hello probe), 5 (teardown ordering protocol), 1 (static `TERMINATE`/`TERM_SIGNAL` atomics drive the shutdown).
- **States** (as derived by readers from file combinations):
  1. *Reserved*: dir exists, nothing in it (spawner created it, `spawn.rs:109`, process not yet at the lock).
  2. *Starting*: `lock` held, no `meta.json` yet (`session.rs:457` → `:472`). Discovery skips it (`discovery.rs:262-265` "still being created").
  3. *Starting/not bound*: lock + meta, no `sock` yet (`session.rs:563-565` runs after PTY spawn and thread start). Client `SocketReachability::NotBound`, treated as usable.
  4. *Live*: lock + meta + sock answering hello.
  5. *Zombie (refusing / wedged)*: lock held, socket refuses or never answers hello. Client-side only (`discovery.rs:52-71`, `not_listening`); the host tries hard never to be here (`session.rs:849-884`, `transport.rs:55-70`).
  6. *Exiting*: lock held, `exiting` marker present, socket still accepting (`session.rs:575-582`), `exited` pushed, 100 ms flush grace (`session.rs:590`).
  7. *Exited (clean record)*: `final.txt` + `exit.json` written (`session.rs:600-601`), sock removed (`:602`), lock released when `run` returns.
  8. *Crashed*: lock released with no `exit.json` (SIGKILL, panic after the lock, OOM). Indistinguishable on disk from a session that died in state 2.
  9. *Killed-by-Shelbi*: any of 6-8 plus a `kill-reason` file written by the client (`discovery.rs:224-242`).
  10. *Reaped*: dir removed by `reap_dead_keeping_recent` (`discovery.rs:310-371`).
  - Representable but invalid: `exit.json` present while lock held (the window between `session.rs:601` and the lock drop; readers special-case it: `is_live` at `discovery.rs:284-286` says dead, `exiting()` at `:207-209` says exiting); `exiting` present without `exit.json` and lock released (host died inside the flush grace: reads as Crashed, with a stale `exiting`); dir with no `lock` file at all (state 1 forever if the process failed before `SessionLock::acquire`, e.g. `check_socket_fits` or `create_dir_all` errors at `session.rs:449-452`); `kill-reason` present on a session that exited on its own first.
- **Transitions / writers:**
  - Spawner: `spawn.rs:106-131` (mkdir, launch detached).
  - Session process: `session.rs:457` lock, `:472` meta, `:563-566` sock, `:582` exiting, `:585-590` exited broadcast + grace, `:593-598` close debouncer, `:600-602` final/exit/unlink sock; `set_meta` rewrite at `:244-258`.
  - Shutdown triggers: child exit (`wait_for_child`, `:677-714`), SIGTERM/SIGHUP handler (`:81-95`), fatal accept error sets `TERMINATE` (`:874-880`).
  - Other crates: `shelbi-client/src/discovery.rs` (`record_kill_reason`/`clear_kill_reason` `:224-235`, reaper `:310-371`), `shelbi-orchestrator/src/session_process_backend.rs:568-586` (`slot_death` maps the files to `SlotDeath::{Exiting, Killed, CleanExit, Crash}`).
- **Where it lives:** `crates/shelbi-session/src/session.rs:432-604`, `layout.rs:1-85`, `lock.rs`, `meta.rs`; readers in `crates/shelbi-client/src/discovery.rs:159-420`.
- **Risk: high.** This is the substrate every agent pane depends on, and its state is reconstructed independently by at least four predicates in another crate (`alive`, `exiting()`, `not_listening()`/`usable()`, `exit_record()`/`kill_reason()`) plus `SlotDeath`. Several past bugs are exactly drift between these readings: `phantom-working-dead-session` (exclusive-probe race, `lock.rs:64-71`; lock-only liveness, `discovery.rs:278-283`), `client-connect-skips-exited-sessions` (the `exiting` marker was added because the Exiting window looked Live, `session.rs:575-581`), `rt-re-entering-a-review-fails-to-attach` and `rt-review-slot-relaunch-loop` (zombies and dead-dir pile-up). Concrete remaining failure modes:
  - **Reaper vs a starting session.** `SessionLock::acquire` creates the `lock` file (`lock.rs:37-43`) and only then `flock`s it (`:46`). A concurrent reaper that sees the file and probes `is_held` in that gap reads it as dead (`discovery.rs:342-351`) and `remove_dir_all`s the directory of a session that is starting. Narrow, but nothing prevents it.
  - **Stranded Reserved dirs.** A process that dies before creating `lock` leaves a dir the reaper never touches (it requires `lock` to exist, `discovery.rs:345-348`) and discovery never lists (no meta). They accumulate silently.
  - **Crash vs pre-meta death are the same disk state** (no `exit.json`, lock free), and `slot_death` returns `Crash` for both; a `kill-reason` from an earlier kill can label a later, unrelated death unless every path remembers `clear_kill_reason` (`session_process_backend.rs:562-564`).
  - **Non-atomic markers.** `meta.json`, `exit.json`, `final.txt` use `std::fs::write` (`session.rs:256, 925, 936, 949`), not the repo's `shelbi_state::atomic_write`. A reader catching a half-written `meta.json` skips a session (`discovery.rs:262-265`); a half-written `exit.json` makes `exit_record()` return `None`, so `slot_death` reads a clean exit as `Crash`. Today only startup writes meta (`set_meta` has no production caller), so the meta case is low; the exit.json case is real at every exit.
  - **Invisible diagnostics.** The host's `eprintln!` lines (fd-limit raise, EMFILE backoff, `session.rs:441-447, 864-871`) go to `/dev/null` because `spawn.rs:242-251` redirects stdio and nothing re-points fd 2, so the "log it rather than spin silently" intent does not reach any file.
- **Recommendation: make explicit.** Define one `SessionDiskState` enum plus a single `fn classify(dir) -> SessionDiskState` in `shelbi-session` (next to `layout.rs`), have the host write markers through `atomic_write` in a documented order, and make discovery, the reaper and `slot_death` consume that classification instead of re-deriving it. Create `lock` and take the flock before anything else is visible (or have the reaper require `meta.json` age > N seconds) to close the reaper race.

### 2. Reverse-forward / ControlMaster health per SSH host

- **Entity:** the hub's `-R` reverse forward to one remote host (what remote workers use to reach `hub.sock`), together with the multiplexed ControlMaster it rides on.
- **Encoding:** 3 (`~/.shelbi/forward-modes.json` `{mode, port}` per host; ControlMaster socket under `~/.shelbi/ssh/`; `master-<host>.lock`), 4 (master liveness via `ssh -O check`, remote landing socket existence/writability/connectability, remote TCP port occupancy, `ControlPersist=600` expiry), 6 (retry with exponential backoff, a 120 s per-host recheck cadence), 1/8 (process-global statics `forward_health`, `forward_check_stamps`, `eperm_logged`), plus the YAML `forward:` pin.
- **States:** an explicit resolver exists, `EffectiveForward::{Unix, Tcp(port), TcpUnbound}` (`shelbi-state/src/ssh_control.rs:587-621`), but the lifecycle is wider and implicit:
  - Unix: Healthy / Broken(`master_not_running` | `landing_socket_missing` | `landing_socket_unwritable` | `forward_connect_refused`) / Inconclusive / MasterOpenFailed / Wedged (→ fallback).
  - Tcp(port): Healthy / Broken / sweep outcome Bound(port) | TransportUnstable(port) | Exhausted.
  - TcpUnbound (pin says tcp, no port yet).
  - Reporting sub-state per host: last signature `"ok"` or `"fail:<detail>"` (`lib.rs:1284-1324`).
  - Representable but inconsistent: record says `Tcp(p)` while the master that bound `p` was dropped (`drop_master` at `lib.rs:1748` runs before the sweep; if the sweep then fails, the record still points at `p` with nothing bound). `TcpUnbound` hands workers and the `-R` argv the **Unix** spec (`lib.rs:277-289`, `ssh_control.rs:710-716`) though the host is in TCP mode.
- **Transitions / writers:**
  - `ensure_reverse_forward` (`lib.rs:1880-1993`): probe → repair → Unix path (`ensure_unix_forward`, `:1467-1551`) or TCP path (`ensure_tcp_forward`, `:1702-1828`); fallback Unix→Tcp on Wedged when auto (`:1955-1974`).
  - `save_host_forward` writes: `lib.rs:1767-1773` (Bound), `ssh_control.rs:637-641` (`effective_forward` clears a TCP record when the pin says unix, as a side effect of a *read*), `ssh_control.rs:658-672` (`reconcile_forward_state` drops every record whose master is not alive).
  - Master drops: `drop_master` from `ensure_unix_forward` (`:1475, 1482`, retry hook `:1499`), `ensure_tcp_forward` (`:1748`), `run_resilient_with_cleanup` (`:686-689`). Master opens: any ordinary `run`/`build_command` via `ControlMaster=auto` (`lib.rs:298-311`), not under the master lock.
  - Other crates: `shelbi-orchestrator/src/poller.rs:634` (`recheck_reverse_forward` on a cadence), `shelbi-cli/src/commands/daemon/serve.rs:793` (startup reconcile with `master_alive`), `shelbi-state/src/ssh_control.rs` (record + resolver + `remote_hub_endpoint` for worker env).
- **Where it lives:** `crates/shelbi-ssh/src/lib.rs:81-167, 237-320, 1006-1090, 1150-1993`; `crates/shelbi-state/src/ssh_control.rs:540-720`; `crates/shelbi-ssh/tests/forward_recheck.rs` pins the probe/repair matrix.
- **Risk: high.** Failure here silently cuts worker→hub messages for every workspace on a host. Concrete failure modes:
  - **The remembered TCP fallback is forgotten on every daemon start.** `reconcile_forward_state(|host, _| master_alive(host))` (`serve.rs:793`) drops any record whose master is not up, which after a reboot or a >600 s idle (`ControlPersist`) is all of them. The next recheck resolves `Unix` again, re-runs the Unix repair on a Tailscale-SSH host, re-wedges and leaks another root-owned socket, then falls back again. The doc comment promises the opposite: "remember the decision so subsequent forwards skip the failing Unix attempt" (`lib.rs:1902-1906`).
  - **Lost update on `forward-modes.json`.** `save_host_forward` is load → modify → `atomic_write` with no file lock (`ssh_control.rs:566-581`). The per-host master lock does not serialize different hosts, and `effective_forward` writes from read paths, so two processes (daemon poller, TUI, CLI) updating different hosts can drop each other's record. The comment says it is "so a concurrent update for a different host isn't lost", which only holds within one process.
  - **Stale port after a failed sweep.** `ensure_tcp_forward` drops the master (releasing the old port) before sweeping (`lib.rs:1744-1748`); on `TransportUnstable`/`Exhausted` it returns an error without clearing the record, so `remote_hub_endpoint` keeps advertising `tcp:127.0.0.1:<old>` to workers and the next ordinary `ssh` reopens a master with `-R <old>` under `ExitOnForwardFailure=no`, which can fail to bind silently.
  - **Masters are opened by anyone, dropped under a lock.** Every multiplexed command opens a master implicitly with whatever `-R` spec it computed at that instant; only repairs take `lock_ssh_master`. A command racing a repair can bring up a master carrying the pre-repair spec. `run_resilient` drops the shared master on any exit-255 with blank or mux-like stderr (`lib.rs:595-612, 686-689`), which ends every other session multiplexed over it (the hazard `ForwardProbe` docs warn about, `lib.rs:1326-1328`).
  - **Event de-duplication is per process.** `forward_health` is a process static, so the daemon, the TUI and each CLI each emit their own "first" failure line to `events.log`.
- **Recommendation: make explicit (small redesign).** Give the per-host forward a single owner (the daemon) with a `ForwardState` enum persisted under a file lock, and a single `transition(host, probe_result)` function. Keep the TCP decision sticky across restarts unless the pin changes (reconcile should drop the *port*, not the *mode*), and clear the port on a failed sweep.

## Medium

### 3. Per-client connection state in the session socket server

- **Entity:** one client connection to a session (`ClientChannel` + its reader/writer threads).
- **Encoding:** 1 (flag soup: `attached: AtomicBool`, `Outbox.resync: bool`, `Outbox.closed: bool`, `caps: AtomicU16`, the reader-local `hello_seen`), 5 (hello → attach → live protocol), 6 (`HELLO_TIMEOUT` 5 s, `CLIENT_WRITE_TIMEOUT` 2 s, `KEEPALIVE_INTERVAL` 20 s).
- **States:** Handshaking (pre-hello, read deadline armed) → Connected (caps set, not attached) → Attached (live) ⇄ ResyncPending (queue overflowed, `resync = true`) → Closed. Detached (after `detach`) is Connected again. Representable but invalid: Attached with `caps == 0` (an `attach` before `hello` is accepted: `handle_frame` does not gate on `hello_seen`, `transport.rs:519-575`, and `Shared::attach` then skips the initial replay because `wants_resync()` is false, `session.rs:266-277`, so the client gets live output with no base); `closed` set by overflow for a non-resync client while the reader keeps running until EOF (`transport.rs:176-182`).
- **Transitions / writers:** `transport.rs:147-149` (caps), `:168-187` (enqueue: overflow → resync or closed), `:191-198` (priority enqueue ignores `resync`), `:200-209` (attach/detach flags + clear), `:212-216` (close), `:395-434` (writer: consumes `resync`, clears queue), `:451-517` (reader: hello deadline), `session.rs:266-283` (attach/detach), `:313-319` & `:332-337` (teardown broadcast/close_all).
- **Where it lives:** `crates/shelbi-session/src/transport.rs:110-517`, `crates/shelbi-session/src/session.rs:260-314`.
- **Risk: medium.** Concrete failure modes:
  - **Priority frames are dropped on resync.** `enqueue_priority` documents replies/keepalives as frames that "must not be dropped" (`transport.rs:189-190`), but both the overflow path (`st.queue.clear()`, `:177`) and the writer's resync branch (`st.queue.clear()`, `:404-407`) discard everything queued, including a pending `SnapshotData`/`InfoData`/`Hello` reply, a `Pong`, or the `Exited` event. A lagging client at session exit therefore sees EOF instead of `exited` (`broadcast_exited` enqueues priority at `:313-318`, the writer then takes the resync branch and clears it), which is indistinguishable from a dropped connection rather than an exit; a client waiting on a snapshot reply waits out its own timeout.
  - **Detach/emit race.** `Shared::detach` flips `attached` and clears the queue without the output gate (`session.rs:280-283`), while `emit_and_feed` checks attachment under the gate (`:372-381`); an output frame can land after the clear and precede the next attach's `Resync`.
  - Past bugs pinned by tests: `rt-review-session-wedges-after-repeated-attaches` (writer blocked forever, `transport.rs:38-53`), `rt-find-the-5s-connection-to-the-review-session` (silent pre-hello peers leaked handlers, `:55-70`).
- **Recommendation: make explicit.** A `ClientPhase` enum (Handshaking, Connected, Attached, Resyncing, Closed) behind the outbox mutex with one transition function; split the outbox into a lossy output lane and a lossless control lane so a resync never discards replies or `Exited`; reject `attach` before `hello`.

### 4. Terminal search session goes stale between `feed` and `refresh_search` (shelbi-term)

- **Entity:** `TerminalView.search: Option<Search>` (`shelbi-term/src/view.rs:51`), inside it `Search { matches, current: Option<usize> }` (`search.rs:43-48`).
- **Encoding:** 1, 5 (`refresh_search` must be called after `feed`), 7 (search UI mode).
- **States:** Closed; Open-empty (`current == None`); Open(i); **Stale** (not represented): matches hold absolute grid lines from an older grid, so every k lines pushed into history shift the real text by k. The invariant "`current` is Some iff `matches` non-empty" holds through run/rerun/select (`search.rs:64-65, 90, 125, 145`); `current()` indexes `matches[i]` unchecked (`:171`).
- **Transitions / writers:** `view.rs:87` (alt-screen switch silently sets `None`), `:98` (resize → `None`), `:329` (start), `:340` (rerun), `:350, :360` (step), `:387` (clear); `search.rs:64-65, 90, 125-131, 145, 179-194`.
- **Risk: medium.** `feed` deliberately does not rescan (`view.rs:78-80`), so highlights and the `search_next`/`search_prev` → `reveal` targets (`view.rs:398-412`) are off by the number of lines scrolled since the last scan. The desktop debounces a refresh (`desktop/src/elements/terminal/mod.rs:1325-1338`, the only caller of `refresh_search` outside the crate); **the TUI never calls it** (`crates/shelbi-tui/src/shell/terminal_view.rs` feeds at 98/102, steps at 180/184), so in the TUI, stepping after new output lands on the wrong lines: a TUI/desktop parity gap. The silent close on alt-screen switch also flips the TUI's derived key-ownership mode (`terminal_view.rs:91`, `scroll_offset() > 0 || search_current().is_some()`) while the TUI's search box may still be open.
- **Recommendation: make explicit.** Have `feed` either shift match lines by the scroll delta or mark the search dirty so stepping rescans first, and report "search closed" out of `feed`. At minimum wire `refresh_search` into the TUI.

### 5. Live selection owned in two places (view mirror vs emulator) (shelbi-term)

- **Entity:** `TerminalView.selection: Option<(SelectionMode, String)>` (`view.rs:50`) mirroring the emulator's `Term::selection`.
- **Encoding:** 1 (two Options), 4 (mirror of emulator state), 8 (the emulator field has many writers).
- **States (view, term):** (None, None) none; (Some, Some) live, sub-state "watched" when it spans ≤ 2,000 lines (`SELECTION_WATCH_LINES`, `view.rs:39`), otherwise the remembered text is empty and in-place overwrites are never detected; (Some, None) **representable but invalid**, transient after the emulator drops the selection, reconciled only on the next `feed` (`view.rs:270-282`); (None, Some) unreachable by construction.
- **Transitions / writers:** `view.rs:196-197` (begin), `:227-228` (update, only if the view half is Some), `:249-250` (clear), `:256`, `:274, :279` (drop-if-changed), `:86` (alt switch), `:97` (resize); `gesture.rs:167, 178, 198, 201, 208, 249`; vendored emulator internals (`vendor/alacritty_terminal/src/term/mod.rs`, e.g. `clear_line` at ~1709).
- **Risk: medium.** (a) The emulator drops any selection overlapping an erased line, even when the same text is redrawn, so a selection over a pane whose renderer erases-and-reprints every frame (Ink, used by Claude Code) is lost on the next frame. (b) The text compare (`view.rs:277-280`) drops a selection over a spinner/timer line. (c) If the selection is lost mid-drag, the gesture stays `Selecting` but `update_selection` returns early (`view.rs:204-206`), and on release `finish_selection` returns `Handled` with no `Copy` (`gesture.rs:244-251`): the highlight vanishes and nothing is copied. (d) A compare-drop happens after the feed and only damages rows whose text changed, so a viewer that repaints by damage can leave stale highlight on other rows. Tests (`view.rs:647-669`) cover only the intended drops.
- **Recommendation: document, then make the drop observable** (have `feed` report "selection lost" so the gesture ends and the viewer repaints); optionally collapse to `enum SelectionState { None, Live { mode, watched: Option<String> } }`.

### 6. Daemon resurrection watchdog (session-side)

- **Entity:** the "should the hub daemon be running?" decision, made by every session host.
- **Encoding:** 4 (daemon single-instance lock held?), 3 (`state.json` `open` flag per project), 6 (15 s + 0-10 s jittered timer, `daemon_watchdog.rs:38-46`), 5 (ordering protocol: quit must clear `open` before sessions exit).
- **States:** (lock held, project open) → Up; (free, open) → Restart; (free, closed) → Intentionally down; (held, closed) → Up/ignored. There is no "stopped on purpose while a project is open" state: `should_start` is a pure function of the two observations (`daemon_watchdog.rs:94-96`).
- **Transitions / writers:** watchdog tick `daemon_watchdog.rs:78-90` spawns `__ensure-daemon` (`:158-179`); `open` written by `shelbi-orchestrator/src/lib.rs:563` (true) and `shelbi-orchestrator/src/quit.rs:542, 596` (false); `stop_daemon` (`shelbi-cli/src/commands/daemon/control.rs:248-253`) assumes projects are already closed (`:196-197`).
- **Where it lives:** `crates/shelbi-session/src/daemon_watchdog.rs`, `crates/shelbi-session/src/session.rs:476-482`.
- **Risk: medium.** Correctness rests on every stop path clearing `open` first. Any path that stops the daemon without that (a crash in quit between daemon stop and `set_project_open(false)`, an operator `daemon stop` on an open project, a future command) is "undone" within ~25 s by every session in the project, each racing to spawn a helper (the bind lock dedups, but N helpers start). The watchdog is also pinned to the session's start-time `$SHELBI_HOME`/`$PATH` and keeps running for the session's whole life; the "manage_daemon: false" flag (`session.rs:131-140`) exists because it raced test env mutation.
- **Recommendation: document.** Write the stop protocol ("clear `open`, then stop") next to `should_start` and in the daemon lifecycle doc, or add an explicit `daemon_stopped_by_user` marker the watchdog honors.

### 7. `tui-smoke.sh` keystroke handshake on fixed sleeps (scripts)

- **Entity:** the TUI under test in CI, driven through its UI modes.
- **Encoding:** 3 (sentinel `statusfile`, `scripts/ci/tui-smoke.sh:43, 89, 115`), 4 (`pgrep -f` liveness, `:93`), 5 (Ctrl+P → Tab → q), 6 (`RENDER_WAIT_SECS=2`, `QUIT_WAIT_SECS=12`, fixed `sleep 0.3` between keys).
- **States:** NotSpawned → Alive-unpainted → Painted → PaletteOpen → SidebarFocused → Exited(status).
- **Transitions:** tmux driver `:131-146`, screen driver `:199-226`, `assert_clean_exit` `:113-122`.
- **Risk: medium (CI flakiness).** Keys go out 0.3 s apart with no check that the palette opened or focus moved (`:143-145, 223-225`); keystrokes forward to the orchestrator session by default (`:17-21`), so on a loaded runner Tab and `q` can land in the stub agent and the run fails at the 12 s timeout. "Painted" is any glyph, and the TUI paints before probing the terminal (`:10-13`). A comment at `:192-195` records an earlier version of this bug ("the old early break ... made this check give up ~0.1s in").
- **Recommendation: document**, better: poll `capture-pane` for the palette before sending Tab.

## Low

### 8. Kitty keyboard flags tracked twice (responder vs emulator)

- **Entity:** the child program's active Kitty keyboard-protocol flags.
- **Encoding:** 4/8 (mirrored state: the responder keeps its own `KittyFlags` atomic and `kitty_stack`, independent of the emulator's `keyboard_mode_stack`).
- **States:** flag byte + a push stack. The emulator keeps separate main and alternate-screen stacks (`replay.rs:64-78` reads `inactive_keyboard_mode_stack`); the responder has one stack and only reacts to `CSI > u`, `CSI < u`, `CSI = u` (`responder.rs:228-262`).
- **Transitions / writers:** `responder.rs:241-261`; readers `responder.rs:233-238` (answers `CSI ? u` queries to the child) and `transport.rs:599` (`info.kitty_flags`).
- **Risk: low-medium.** After an alt-screen switch (or any reset the emulator honors and the responder's three-case parser does not), the responder can answer a `CSI ? u` query with flags the emulator (and every client's replayed emulator) no longer has, so a program that probes the protocol on start can pick the wrong key encoding. `info.kitty_flags` has no consumer today (grep finds only the proto definition), which limits the blast radius.
- **Recommendation: make explicit (cheap).** Answer `CSI ? u` from the emulator's mode under the emulator lock instead of a second tracker, and drop `KittyFlags` or derive it.

### 9. `MouseGesture` drag lifecycle has no cancel (shelbi-term)

- **Entity:** `MouseGesture.drag: DragState` (`gesture.rs:62-76, 81`), an explicit enum `Idle | Pressed{col,row,forward_click,block} | Selecting | Program`.
- **Encoding:** explicit enum plus 7 (click-count gestures) and 5 (Press → Drag → Release order assumed); the implicit part is the cross-object invariant with SELECTION.
- **Transitions:** all in `gesture.rs`: `:98` new → Idle; `:169` Alt-press while reporting → Program; `:180` double/triple click → Selecting; `:183-188` press → Pressed; `:203` first Drag → Selecting; `:215` Release → Idle (`mem::replace`).
- **Risk: low.** No `cancel()`: a lost Release (button up outside the window, focus loss, the pane's view swapped to another session) leaves `Selecting`/`Program`/`Pressed` until the next press. Meanwhile `is_selecting()` stays true (the desktop keeps capturing motion), `Program` forwards drags with no matching press, and a stale `Pressed{forward_click}` released into a swapped view sends a click to the new program. `Pressed` stores viewer coordinates resolved only at the first drag (`:197`), so a placement change in between moves the anchor.
- **Recommendation: leave**, add `cancel()` for focus loss / view swap, and document that `Selecting` does not imply a selection exists.

### 10. Scroll-follow mode with a redundant `Scrollback` mirror (shelbi-term)

- **Entity:** `TerminalView.scrollback: Scrollback { offset }` (`view.rs:45`, `scrollback.rs:16-18`) duplicating the emulator's `display_offset`.
- **Encoding:** 1, 4 (mirror), 7 (follow vs scrolled back).
- **States:** Following (offset 0), ScrolledBack(n); gated by `scrollback_enabled()` on the alt screen (`view.rs:117-119`).
- **Writers:** `view.rs:84` (re-mirror after feed), `:95` (resize **clamps** instead of re-mirroring), `:133` (`apply_scroll`), `:110-112` (`on_user_input` → bottom); the emulator moves the real offset on reflow (`vendor/alacritty_terminal/src/grid/resize.rs`). Readers: `view.rs:141-142, 152-153` use the mirror as the scroll base; every other reader uses `emu.display_offset()` directly.
- **Risk: low.** After a resize while scrolled back, the mirror differs from the emulator's offset, so the next wheel/PageUp computes its target from a stale base and the view jumps by the drift. Doc drift: `scrollback.rs:11-12` says new output resets to the bottom, `view.rs:72-74` says new output never yanks a scrolled-up view. `Scrollback::reset/to_top/to_bottom/is_active/at_bottom` are unused.
- **Recommendation: redesign (trivial).** Drop the field and derive the base from `emu.display_offset()` (or re-mirror in `resize`); fix the module doc.

### 11. `install.sh` decides a daemon restart from a status string (scripts)

- **Entity:** whether the running hub daemon is restarted onto the newly installed binary.
- **Encoding:** 2 (stringly marker), 4 (mirrors process state), 3 (plugin staging dir).
- **States:** NotRunning / Running / `--no-daemon` (`scripts/install.sh:21-24`).
- **Transitions:** `install.sh:160-178` greps the output of `shelbi daemon status` for `shelbi daemon: running`, the literal printed at `crates/shelbi-cli/src/commands/daemon/lifecycle.rs:301`. Nothing pins that string.
- **Risk: low-medium.** Rewording the status line silently skips the restart, which is exactly the "stale binary hides merged fixes" failure the script names (`:157-158`). The probe runs the *new* binary, which bakes a possibly different `SHELBI_DEFAULT_ROOT` (`:106`), so a user who picked a new root at the prompt has the old daemon missed. Plugin staging (`:124-144`) has a documented removed-but-not-renamed window covered by the embedded fallback.
- **Recommendation: document**, or switch to an exit code / JSON status.

### 12. `wait-for-apt-consistency.sh` polls CDN state (scripts)

- **Entity:** whether the hosted APT repo's `InRelease` and index files agree.
- **Encoding:** 4 (mirrors edge/CDN state), 6 (deadline + interval retry).
- **States:** InRelease unfetchable / path not listed / served file unfetchable / size-hash mismatch / Consistent (exit 0) / TimedOut (exit 1).
- **Transitions:** loop `scripts/release/wait-for-apt-consistency.sh:103-155`, deadline set at `:100`, checked only between attempts (`:146`).
- **Risk: low-medium.** `curl -fsSL` has no `--max-time`/`--connect-timeout` (`:73-80`), so one stalled fetch blocks past the 300 s deadline until the Actions job timeout; one consistent read with `no-cache` headers exits 0 (`:140-143`) while the following `apt-get` (`.github/workflows/release.yml` ~612) may hit another edge. The `release.yml:600-606` comment records the earlier failure (a 5×15 s apt-get loop gave up before convergence).
- **Recommendation: leave**, plus curl timeouts and optionally two consecutive consistent reads.

## Minor

- **Resize debouncer** (`shelbi-session/src/session.rs:151-167, 421-430, 781-818`): `Sizing { active, per_client, target, generation, applied, closed }` with a generation-counter debounce. Contained in one mutex and one thread; the "PTY follows the last client to send input" rule is clear. Leave.
- **Session shutdown flags** (`session.rs:66-95`): process statics `TERMINATE`/`TERM_SIGNAL`/`TERM_SENDER`, first-signal-wins via CAS. Correct and small; part of finding 1's teardown.
- **`info.child_running` is always `true`** (`transport.rs:604`) even while the session is in its Exiting window. Harmless today.
- **`askpass` paused deadline** (`shelbi-ssh/src/askpass.rs:405-488`): `PromptWatch` (open helper connections) + `AskingClock` (counted time stops while a dialog is up). Explicit and local. `run_until` kills only the `ssh` child, not its group (`:560-563`), so an open `osascript` dialog can outlive a timed-out step. Leave.
- **`CancelScope`** (`shelbi-ssh/src/lib.rs:20-67`): thread-local ambient deadline/cancel with an RAII restore guard. Clean; note that the scope silently changes `run`'s behavior for any callee on that thread.
- **`MuxFailure` classification** (`lib.rs:551-612`): a blank-stderr exit 255 is treated as a transport loss, but a remote command that itself exits 255 silently is indistinguishable and triggers a shared-master drop in `run_resilient`. Feeds finding 2.
- **`eperm_logged` / `forward_check_stamps`** (`lib.rs:1254-1275, 1423-1448`): process-global "already logged" / "last checked" maps, never pruned; per-process, so de-dupe does not span daemon + TUI + CLI. Part of finding 2.
- **`ClientChannel` caps** (`transport.rs:84-108`): capability bits re-set on every hello; a second hello mid-stream changes what an attached client receives. Theoretical.
- **shelbi-term emulator damage** (`shelbi-term/src/emulator.rs:283-295`): damage accumulates until taken; selection/search changes never damage (`view.rs:101-104`), so viewers must repaint those themselves (call-order protocol, ties to finding 5).
- **shelbi-term `size` mirror** (`emulator.rs:103, 141-147`): stores the raw size while the term uses `non_zero()`; a 0-row size gives `page_up` a 0 scroll.
- **`MouseModes { reporting, drag, motion, sgr }`** (`shelbi-term/src/input.rs:458-486`): implied motion ⇒ drag ⇒ reporting ordering is not enforced, but only `emulator.rs:321-329` builds it. Leave. (Its `KeyEncoding` doc points at a nonexistent `KeyEncoding::from_term_mode`, `input.rs:123-124`.)
- **Golden record mode** (`shelbi-term/tests/key_encoding_golden.rs:180`, `SHELBI_WRITE_KEY_GOLDEN`) and **`check-test-home.sh --verify`** (`$HOME/.shelbi` presence as the marker): trivial env/file toggles.
- **`build-apt-repo.sh`** (`:86, 103-116`): pool accumulates whatever the repo dir holds; `InRelease` removed then re-signed (non-atomic, local only).
- **`update-homebrew-cask.rb:77-78`**: comment says the cask depends on the formula "at the same version" but `depends_on formula:` is unpinned; the pairing lives only in the tap.
- **shelbi-palette**: `Entry.id` prefixes (`view:`, `agent:`) duplicate `kind` (identifiers, not states). Header comment says Ctrl+Space while CLAUDE.md and tui-smoke use Ctrl+P.
- **shelbi-agent flag builders** (`shelbi-agent/src/lib.rs:188-390`): `with_permission_mode`/`with_continue`/`with_model`/`with_reasoning_effort` are idempotent-ish transforms whose result depends on what the YAML already carries (e.g. an existing `--permission-mode` wins even when it differs, `:848`). Pure; not a machine.

## Cross-slice leads

- **Session directory lifecycle (finding 1)** is read and partly written outside this slice: `crates/shelbi-client/src/discovery.rs` (`list`, `is_live`, `exiting()`, `reachability`/`not_listening`/`usable`, `record_kill_reason`/`clear_kill_reason`, `reap_dead_keeping_recent`, `choose_session`) and `crates/shelbi-orchestrator/src/session_process_backend.rs:560-600` (`slot_death` → `SlotDeath`, `name_wedged`). Whoever surveys orch-workspace / cli-daemon should treat these as one machine with finding 1; the client's attach/reconnect loop (`session-lost` / takeover, see `shelbi-orchestrator` supervision) is the consumer of the Exiting/Crash distinction.
- **Reverse forward (finding 2):** `crates/shelbi-state/src/ssh_control.rs:540-720` (`forward-modes.json`, `effective_forward`, `reconcile_forward_state`, `remote_hub_endpoint`), `crates/shelbi-state/src/lib.rs:1190` (`lock_ssh_master`), `crates/shelbi-cli/src/commands/daemon/serve.rs:793` (startup reconcile), `crates/shelbi-orchestrator/src/poller.rs:634` (recheck cadence), and worker launch code that bakes `SHELBI_HUB_ADDR` from `remote_hub_endpoint` (orchestrator dispatch). Relay connections (`shelbi-client` relay `session` service, two-hop attach) ride the same ControlMaster and die when `run_resilient`/repair drops it.
- **Daemon watchdog (finding 6):** `state.json` `open` flag writers `crates/shelbi-orchestrator/src/lib.rs:563` and `quit.rs:542, 596`; `shelbi_state::daemon_lock_held` / `ensure_daemon_running` (`crates/shelbi-state/src/daemon_lifecycle.rs`); `stop_daemon` in `crates/shelbi-cli/src/commands/daemon/control.rs:196-253` assumes projects already closed. Belongs with the cli-daemon slice's daemon lifecycle.
- **Terminal view (findings 4, 5, 9, 10):** `crates/shelbi-tui/src/shell/terminal_view.rs` (no `refresh_search`; derived `in_scrollback` mode at :91; owns a `MouseGesture` without click counts), `desktop/src/elements/terminal/mod.rs` (`MouseGesture`, `is_selecting`, search refresh debounce :1325-1338, `on_user_input`), `desktop/src/elements/terminal/layout.rs` (consumes `take_damage`: check the stale-highlight risk), `crates/shelbi-cli/src/commands/session_attach.rs` (`on_user_input`). Vendored emulator writers: `vendor/alacritty_terminal/src/term/mod.rs` (selection), `grid/resize.rs` (`display_offset`).
- **install.sh (finding 11):** `crates/shelbi-cli/src/commands/daemon/lifecycle.rs:290-301` status text.
- **CI/release (findings 7, 12):** `.github/workflows/app-ci.yml` (tui-smoke), `.github/workflows/release.yml:600-612`, `release-apt.yml`.
- **Desktop on-disk markers** named in the cask's zap list (`scripts/release/update-homebrew-cask.rb:84-85`): `~/.shelbi/desktop-update-check.json`, `projects/*/desktop-window.json` (desktop-shell slice).
- **Agent process control:** shelbi-agent has no lifecycle state; agent launch/resume/readiness (`ResumeStrategy`, `needs_claude_readiness_probe`, `MessageChannel`) are consumed by the orchestrator's dispatch and supervision code, which is where the agent-process state machine lives (orch-workspace slice).