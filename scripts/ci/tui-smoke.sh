#!/usr/bin/env bash
# TUI nesting smoke test: launch the single-process `shelbi` TUI inside a
# terminal multiplexer and confirm it comes up, renders, and tears down cleanly
# without hanging.
#
# Usage: scripts/ci/tui-smoke.sh <tmux|screen>
#   $SHELBI_BIN   path to the `shelbi` binary under test (required)
#
# What it hard-asserts, and why it is shaped this way:
#   * RENDER: the nested TUI paints a non-empty frame within ~2s. The shell
#     draws its first frame before probing the terminal or waiting on the daemon
#     (`rt-tui-headless-startup-block`), so a headless PTY sees a frame within
#     milliseconds; no painted glyphs after 2s means that path regressed (or the
#     binary crashed on a nested launch, e.g. broken terminal setup). tmux reads
#     the frame with `capture-pane`; Screen reads it from an output log (its
#     `hardcopy` misses an alternate-screen app, the log does not).
#   * CLEAN EXIT: the quit sequence is delivered and the process exits 0 within
#     the window. The shell opens on the orchestrator session, where keystrokes
#     forward to the agent, so quitting is driven the way a user would: Ctrl+P
#     opens the command palette, Tab moves focus to the sidebar, and `q` closes
#     the UI. The loop sets `should_quit`, breaks, and returns Ok, and the
#     launcher records the real exit status in a sentinel file both muxes assert
#     on.
#
# Every wait is bounded and a trap reaps the multiplexer and child processes, so
# this script always terminates regardless of the TUI's state.

set -uo pipefail

MUX="${1:?usage: tui-smoke.sh <tmux|screen>}"
: "${SHELBI_BIN:?set SHELBI_BIN to the shelbi binary under test}"

RENDER_WAIT_SECS=2   # how long to wait for the first non-empty frame
QUIT_WAIT_SECS=12    # how long to wait for the quit key to end the process

PROJECT="shelbismoke"
MARKER="shelbi --project ${PROJECT}"

workdir="$(mktemp -d)"
home="${workdir}/home"
stub="${workdir}/bin"
repo="${workdir}/${PROJECT}"
statusfile="${workdir}/exit-status"
mkdir -p "${home}" "${stub}" "${repo}"

cleanup() {
  [ -n "${SOCK:-}" ] && tmux -S "${SOCK}" kill-server >/dev/null 2>&1
  screen -S "${PROJECT}" -X quit >/dev/null 2>&1
  pkill -f "${home}" >/dev/null 2>&1
  rm -rf "${workdir}"
}
trap cleanup EXIT

fail() { echo "::error::tui-smoke (${MUX}): $*"; exit 1; }

# A stub agent so the onboarding runner-detection passes and the orchestrator
# session has a child to run; it just stays alive.
cat > "${stub}/claude" <<'STUB'
#!/bin/sh
case "$1" in
  --version) echo "claude 0.0.0 (ci-smoke-stub)"; exit 0 ;;
  *) exec sleep 3600 ;;
esac
STUB
chmod +x "${stub}/claude"

export PATH="${stub}:${PATH}"
export SHELBI_HOME="${home}"

# A minimal git repo and a registered project so `shelbi --project` opens the
# dashboard rather than onboarding.
(
  cd "${repo}"
  git init -q
  git config user.email ci@shelbi.local
  git config user.name "shelbi CI"
  git commit -q --allow-empty -m init
  "${SHELBI_BIN}" init -y --runner claude >/dev/null 2>&1
) || fail "project setup failed"

# The command each multiplexer runs: the real TUI, in the project worktree.
# It does NOT `exec` the binary, so the quit exit code is captured into the
# sentinel file the assertions below read. A crash or signal lands there too.
launcher="${workdir}/launch.sh"
cat > "${launcher}" <<LAUNCH
#!/bin/sh
cd "${repo}"
"${SHELBI_BIN}" --project ${PROJECT}
echo "\$?" > "${statusfile}"
LAUNCH
chmod +x "${launcher}"

alive() { pgrep -f "${MARKER}" >/dev/null 2>&1; }
# A frame is "rendered" once the captured screen has any non-whitespace glyph
# (tmux `capture-pane` returns clean text, so this is enough there).
has_glyph() { grep -q '[^[:space:]]'; }
# Strip the common terminal escapes from a raw Screen output log (CSI sequences,
# charset selects, and the `ESC =`/`ESC >` keypad toggles) so only the painted
# text is left.
screen_strip() {
  LC_ALL=C sed \
    -e 's/'$'\033''\[[0-9;?]*[ -/]*[@-~]//g' \
    -e 's/'$'\033''[()*+][A-Za-z0-9]//g' \
    -e 's/'$'\033''[=>]//g'
}
# A painted frame leaves an alphanumeric glyph once the escapes are stripped.
screen_log_has_glyph() {
  screen_strip | tr -cd '[:alnum:]' | grep -q .
}

# Wait up to QUIT_WAIT_SECS for the launcher to record an exit status, then
# assert it is a clean 0. Shared by both muxes.
assert_clean_exit() {
  for _ in $(seq 1 $((QUIT_WAIT_SECS * 2))); do
    [ -f "${statusfile}" ] && break
    sleep 0.5
  done
  [ -f "${statusfile}" ] || fail "quit key did not end the process within ${QUIT_WAIT_SECS}s"
  status="$(cat "${statusfile}")"
  [ "${status}" = "0" ] || fail "quit key produced exit status '${status}', expected 0"
  echo "tui-smoke (${MUX}): clean exit 0 on the quit key"
}

case "${MUX}" in
  tmux)
    SOCK="${workdir}/tmux.sock"
    tmux -S "${SOCK}" new-session -d -s "${PROJECT}" -x 120 -y 40 "${launcher}" \
      || fail "could not start a tmux session"
    tmux -S "${SOCK}" set-option -t "${PROJECT}" remain-on-exit on >/dev/null 2>&1

    rendered=0
    for _ in $(seq 1 $((RENDER_WAIT_SECS * 10))); do
      if alive && tmux -S "${SOCK}" capture-pane -t "${PROJECT}" -p 2>/dev/null | has_glyph; then
        rendered=1
        break
      fi
      sleep 0.1
    done
    [ "${rendered}" = "1" ] || fail "no rendered frame within ${RENDER_WAIT_SECS}s of a nested tmux launch"
    echo "tui-smoke (tmux): rendered a non-empty frame within ${RENDER_WAIT_SECS}s"

    # Ctrl+P (palette) -> Tab (focus sidebar) -> q (close UI).
    tmux -S "${SOCK}" send-keys -t "${PROJECT}" C-p; sleep 0.3
    tmux -S "${SOCK}" send-keys -t "${PROJECT}" Tab; sleep 0.3
    tmux -S "${SOCK}" send-keys -t "${PROJECT}" q
    assert_clean_exit
    ;;

  screen)
    export SCREENDIR="${workdir}/screen"
    mkdir -p "${SCREENDIR}"; chmod 700 "${SCREENDIR}"
    # Capture the window's output by logging it to a file rather than scraping
    # the rendered buffer: `hardcopy` only copies the normal screen, so it misses
    # an alternate-screen app (the shell enters the alternate screen), while the
    # log is the raw byte stream the TUI wrote regardless of how Screen renders
    # it. `flush 1` keeps the log at most ~1s behind (the default is 10s).
    log="${workdir}/screen.log"
    rc="${workdir}/screenrc"
    cat > "${rc}" <<RC
logfile ${log}
logfile flush 1
logtstamp off
RC
    screen -c "${rc}" -L -dmS "${PROJECT}" "${launcher}" \
      || fail "could not start a screen session"

    # Gather every byte Screen may have logged. The rc points logging at ${log},
    # but a Screen that ignores the directive falls back to `screenlog.N` in the
    # window's working directory (the repo), so read both. This keeps the glyph
    # check from missing a frame that merely landed in a different file.
    screen_capture() {
      [ -f "${log}" ] && cat "${log}"
      cat "${repo}"/screenlog.* 2>/dev/null
      return 0
    }

    # Dump what Screen is doing when the render check gives up. GNU Screen on the
    # runner behaves differently from macOS's, so make a failure self-describing
    # rather than guessing: the detached session list, the SCREENDIR socket, and
    # the captured log's size and stripped head all land in the CI output.
    screen_diag() {
      echo "tui-smoke (screen): --- diagnostics ---"
      echo "tui-smoke (screen): screen -ls:"; screen -ls 2>&1 | sed 's/^/tui-smoke (screen):   /'
      echo "tui-smoke (screen): SCREENDIR (${SCREENDIR}):"; ls -la "${SCREENDIR}" 2>&1 | sed 's/^/tui-smoke (screen):   /'
      echo "tui-smoke (screen): repo screenlog files:"; ls -la "${repo}"/screenlog.* 2>&1 | sed 's/^/tui-smoke (screen):   /'
      echo "tui-smoke (screen): captured $(screen_capture | wc -c | tr -d ' ') bytes; stripped head:"
      screen_capture | screen_strip | head -c 2000 | sed 's/^/tui-smoke (screen):   /'
      echo
      echo "tui-smoke (screen): pane alive=$(alive && echo yes || echo no)"
    }

    # Poll the FULL window for a painted frame. Do not break early when the pane
    # is not yet alive: Screen spawns the detached child asynchronously, so for
    # the first fraction of a second `alive` is false even on a healthy launch
    # (the old early break here is what made this check give up ~0.1s in). The
    # extra second past RENDER_WAIT_SECS absorbs the 1s log-flush lag, so a frame
    # the TUI actually paints within 2s is still observed; the `shelbi` process
    # exiting on its own (crash) ends the wait early with diagnostics.
    rendered=0
    for _ in $(seq 1 $(((RENDER_WAIT_SECS + 1) * 10))); do
      # A painted frame leaves printable glyphs (the sidebar label, status, and
      # headers) in the log once terminal escapes are stripped; a launched-but-
      # blank TUI emits only setup escapes and no printable text.
      if screen_capture | screen_log_has_glyph; then
        rendered=1
        break
      fi
      # Only bail before the deadline if the launcher recorded an exit: the pane
      # died on its own, so no frame is coming. A not-yet-spawned pane (no status
      # file, not alive) keeps waiting.
      [ -f "${statusfile}" ] && break
      sleep 0.1
    done
    if [ "${rendered}" != "1" ]; then
      screen_diag
      fail "no rendered frame within ${RENDER_WAIT_SECS}s of a nested screen launch"
    fi
    echo "tui-smoke (screen): rendered a non-empty frame within ${RENDER_WAIT_SECS}s"

    # Ctrl+P (0x10, palette) -> Tab (0x09, focus sidebar) -> q (close UI). `-p 0`
    # addresses the window explicitly, which older Screen needs for `stuff` to
    # land on a detached session.
    screen -p 0 -S "${PROJECT}" -X stuff "$(printf '\020')" >/dev/null 2>&1; sleep 0.3
    screen -p 0 -S "${PROJECT}" -X stuff "$(printf '\011')" >/dev/null 2>&1; sleep 0.3
    screen -p 0 -S "${PROJECT}" -X stuff 'q' >/dev/null 2>&1
    assert_clean_exit
    ;;

  *)
    fail "unknown multiplexer '${MUX}' (expected tmux or screen)"
    ;;
esac

echo "tui-smoke (${MUX}): ok"
