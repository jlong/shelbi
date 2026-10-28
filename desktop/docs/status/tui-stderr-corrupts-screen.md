# tui-stderr-corrupts-screen: nothing written over the TUI's frame

## The change

While GitHub was flaky, the TUI's sidebar showed debris it never drew:
fragments like `arch` over the header and stacked copies of
`transient GitHub error … (attempt 1/2)` and the footer's version line. The
`gh` retry notifier (`shelbi_state::gh_retry`, `default_notify`) printed each
wait to stderr, and in the TUI stderr is the terminal the frame is drawn on
(raw mode, alternate screen). The line landed at the cursor, wrapped across
the sidebar and could scroll the screen; ratatui only repaints cells its own
buffer thinks changed, so the debris stayed until those cells changed again.

Now nothing reaches that terminal from outside the frame, and if something
ever does, the screen recovers.

## Retry notices (`shelbi_state::gh_retry`)

- **A process-wide sink.** `set_retry_notice_sink` /
  `clear_retry_notice_sink` (re-exported from `shelbi_state` with
  `RetryNotice`, `RetryKind`, `NoticeSink`). The production policies' notifier
  hands each wait to the installed sink, else prints it to stderr as before,
  so every CLI command (`shelbi issue …`, migrations) is unchanged.
- **`RetryNotice::message()`**: the line without the `shelbi: ` prefix
  (`transient GitHub error; retrying in 1s (attempt 1/2)`), shared by stderr,
  the TUI and the desktop.
- **`notify_retry_for_test`** (test-support): delivers a notice exactly as the
  production policies do, for a client's test.

**The TUI** installs a sink at startup (`shell/stray_output.rs`,
`install_retry_notices`) that logs the line to `~/.shelbi/logs/tui.log` and
sends it to the run loop. The newest notice takes the status band (the main
area's bottom row, and Settings' status) for six seconds or the whole wait if
longer, then whatever status was showing comes back; the notice never
replaces `status`. The sink is removed when the loop ends. Under an open
review or workspace panel the shell's status band is hidden (as before), so
the notice is in `tui.log` only there.

**The desktop** installs a sink in `init_logging` that emits the line as a
`tracing` warning, so it follows the app's log filter like every other line.

## Everything else on stderr

The audit of writers that can fire while the TUI runs: the retry notifier
(above); the board's task-file warnings in `shelbi_state` (`skipping
malformed task file …`, read on the refresher threads); many `eprintln!`s in
`shelbi_orchestrator` that palette commands reach in-process; and any child
process (`gh`, `git`, `ssh`) that inherits stderr. `tracing` already went to
`tui.log` (`init_tracing` in `shelbi-cli`). Rather than chase each one, **fd 2
points at `tui.log` while the alternate screen is up**
(`stray_output::redirect_stderr`, called by `enter_terminal_modes`; `/dev/null`
if the log can't be opened). `restore_terminal` puts the terminal's stderr
back (`restore_stderr`), so it is restored on quit, on an error exit, before
the user's editor runs (and redirected again after), before a re-exec, before
the detach note prints, and in the panic hook before the panic message
prints. Children started meanwhile inherit the log, not the terminal. The
saved terminal descriptor is close-on-exec.

The migration prompt in `shelbi_tui::run_main` runs before the alternate
screen and still uses the terminal, as it must.

## Full repaint

`ShellState::full_redraw` makes the next frame clear the terminal and forget
the last frame (`Terminal::clear`), so every cell is painted again. It is set
by:

- **Ctrl+L**, the `FocusMain` chord (decided with the user, `q-001`: no new
  chord). Ctrl+L still moves focus to the main pane and now also repaints, as
  Ctrl+L conventionally does. The repaint follows the binding, so a user who
  rebinds `focus_main` in `keys.yaml` moves the repaint with it, and it works
  wherever focus is, over an overlay or Settings too. The action's help text
  says so: "Focus the main pane (right) and redraw the screen", label "Focus
  main pane and redraw" (Settings ▸ Keybindings in both clients). On the
  desktop the chord also refreshes the window.
- **a resize**, and **focus regained** (focus events were already enabled).

## Verified

- `shell::tests::stray_output`: a transient retry fired from a background
  thread during a TUI session shows on the status band's row of a
  `TestBackend` frame and writes nothing to stderr (checked in a child test
  process with `--nocapture`, against a control run without the sink that
  does print); a notice shows over the status and gives it back; Ctrl+L, a
  resize and focus regained each wipe text scribbled into the terminal behind
  ratatui's back, which an ordinary frame leaves; Ctrl+L still focuses main
  and repaints over the palette.
- `shell::stray_output::tests`: fd 2 goes to the file until restored, and a
  second redirect still restores the terminal.
- `gh_retry` tests: the message text, and an installed sink taking the
  production notices.
- Root build, clippy and tests; the desktop's build, clippy and tests.
