#!/usr/bin/env bash
# Concurrent clients on one session: the desktop app, the TUI, and
# `shelbi attach`, at three different sizes, typing in turn.
#
# Usage: desktop/scripts/multiclient-check.sh
#   $SHELBI_BIN    the `shelbi` CLI (default: `shelbi` on PATH)
#   $DESKTOP_BIN   shelbi-desktop built with `--features bench`
#                  (default: desktop/target/release/shelbi-desktop)
#   $KEEP=1        keep the sandbox (and its tmux server) for a look afterwards
#
# Everything runs in a sandboxed Shelbi root under /tmp (its own daemon,
# sessions, and a private tmux server), with the hub's socket variables unset,
# so nothing touches ~/.shelbi or the live hub. Teardown signals only pids
# read from the sandbox's own files.
#
# The session runs bash. Each round one client types `clear; echo
# typed-in-<client>-<round>` + Enter, which makes it the session's active
# client: the PTY takes that client's size (debounced) and bash reflows. Then
# for every client:
#
#   * the session's size is the typing client's viewport;
#   * the desktop app (a real window, `shelbi-desktop --bench-client`, typing
#     through gpui's key dispatch and input handler) shows exactly the
#     session's screen: its grid's text equals the session's own snapshot,
#     and what it paints (the grid letterboxed or clipped into its viewport)
#     equals that snapshot placed by the shared viewport rules;
#   * `shelbi attach` (80x24 in tmux) shows that same placement of the
#     snapshot, line for line;
#   * the TUI (150x45 in tmux) shows it as a block inside its main area.
#
# Rounds: desktop, TUI, attach, then desktop again (so each client sees the
# session both at its own size and at the others'). Exit 0 when every check
# passes; a failure prints the screens it compared.

set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
SHELBI_BIN="${SHELBI_BIN:-$(command -v shelbi)}"
DESKTOP_BIN="${DESKTOP_BIN:-${here}/../target/release/shelbi-desktop}"
[ -x "${SHELBI_BIN}" ] || { echo "no shelbi CLI (set SHELBI_BIN)"; exit 2; }
[ -x "${DESKTOP_BIN}" ] || { echo "no ${DESKTOP_BIN}: cargo build --release --features bench"; exit 2; }

R="$(mktemp -d /tmp/shm.XXXX)"   # short: session socket paths must fit 104 bytes
PROJECT=mc
REPO="${R}/repo"
CTL="${R}/ctl"
TMUX_SOCK="${R}/tmux.sock"
DESKTOP_SIZE=100x30
ATTACH_W=80; ATTACH_H=24
TUI_W=150; TUI_H=45
failures=0

# Every command runs against the sandbox and never sees the hub.
sb() {
  env -u SHELBI_HOME -u SHELBI_PROJECT -u SHELBI_SESSION -u SHELBI_MANAGED_CONTEXT \
    -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_REMOTE_HUB_SOCK -u SHELBI_HUB_ADDR \
    -u TASK_ID -u PROJECT SHELBI_ROOT="${R}" "$@"
}
# sb_bg <log> <command...>: `sb` in the background; its pid in `bg_pid`.
sb_bg() {
  local log="$1"; shift
  env -u SHELBI_HOME -u SHELBI_PROJECT -u SHELBI_SESSION -u SHELBI_MANAGED_CONTEXT \
    -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_REMOTE_HUB_SOCK -u SHELBI_HUB_ADDR \
    -u TASK_ID -u PROJECT SHELBI_ROOT="${R}" "$@" >"${log}" 2>&1 &
  bg_pid=$!
}
tm() { tmux -S "${TMUX_SOCK}" "$@"; }

desktop_pid=""
cleanup() {
  if [ "${KEEP:-0}" = "1" ]; then
    echo "kept sandbox ${R} (tmux -S ${TMUX_SOCK} attach)"
    return
  fi
  tm kill-server >/dev/null 2>&1
  # The desktop client, the sandbox daemon, and the sessions: only pids from
  # the bench's own launch and the sandbox's own files (`shelbi.pid` is
  # "<pid> <start time> <version>").
  pids="${desktop_pid}"
  [ -f "${R}/shelbi.pid" ] && pids="${pids} $(awk '{print $1}' "${R}/shelbi.pid")"
  for m in "${R}"/sessions/*/meta.json; do
    [ -f "$m" ] || continue
    pids="${pids} $(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("pid",""))' "$m")"
  done
  # Anything else started in the sandbox (the daemon the app or the TUI
  # launched, its sessions): processes whose environment has exactly this
  # SHELBI_ROOT. The pattern travels in awk's environment, never its argv, so
  # neither awk nor ps matches itself; the hub's processes don't carry it.
  pids="${pids} $(ps eww -A -o pid=,command= 2>/dev/null \
    | PAT="SHELBI_ROOT=${R}" awk '{ if (index($0 " ", " " ENVIRON["PAT"] " ")) print $1 }')"
  for p in ${pids}; do kill "$p" 2>/dev/null; done
  for _ in $(seq 1 30); do
    alive=""; for p in ${pids}; do kill -0 "$p" 2>/dev/null && alive="${alive} $p"; done
    [ -z "${alive}" ] && break; sleep 0.1
  done
  for p in ${alive}; do kill -9 "$p" 2>/dev/null; done
  rm -rf "${R}"
}
trap cleanup EXIT

fail() { echo "FAIL  $*"; failures=$((failures + 1)); }
ok() { echo "ok    $*"; }

# ---- the sandbox project ------------------------------------------------------
mkdir -p "${R}/projects" "${REPO}" "${CTL}"
git -C "${REPO}" init -q -b main
git -C "${REPO}" -c user.email=mc@shelbi.local -c user.name=mc commit -q --allow-empty -m init
# The orchestrator "agent" is `sleep`, so no real agent runs; the long poll
# interval keeps the daemon from reaping our hand-made workspace session.
cat > "${R}/projects/${PROJECT}.yaml" <<YAML
name: ${PROJECT}
repo: ${REPO}
default_branch: main
workspace_poll_interval_secs: 86400
orchestrator: { runner: noop }
agent_runners:
  noop: { command: sleep, flags: ["1000000"] }
machines:
  - { name: local, kind: local, work_dir: ${REPO} }
workspaces:
  - { name: dev, machine: local }
YAML
cat > "${R}/bashrc" <<'RC'
PS1='mc$ '
shopt -s checkwinsize
RC

sb "${SHELBI_BIN}" session new --name "${PROJECT}/ws/dev" --cwd "${REPO}" --cols 90 --rows 28 -- \
  /bin/bash --noprofile --rcfile "${R}/bashrc" -i >/dev/null || { echo "session new failed"; exit 1; }

session_size() {
  sb "${SHELBI_BIN}" session ls 2>/dev/null | awk -v n="${PROJECT}/ws/dev" '$1==n && $3=="live" {print $4}'
}
for _ in $(seq 1 100); do [ -S "$(ls "${R}"/sessions/*/sock 2>/dev/null | head -1)" ] && break; sleep 0.1; done

snapshot() { sb "${SHELBI_BIN}" session snapshot "${PROJECT}/ws/dev" 2>/dev/null; }

# ---- the three clients ------------------------------------------------------------
# 1. The desktop app: a real window, its pane attached through the app's own
#    connect, driven through a control directory.
# `sb_bg`, not `sb ... &`: backgrounding a shell function forks a subshell, and
# `$!` would be that subshell's pid rather than the app's.
sb_bg "${R}/desktop.log" "${DESKTOP_BIN}" --bench-client "${PROJECT}/ws/dev" "${DESKTOP_SIZE}" "${CTL}"
desktop_pid="${bg_pid}"
for _ in $(seq 1 300); do [ -f "${CTL}/ready" ] && break; sleep 0.1; done
[ -f "${CTL}/ready" ] || { echo "the desktop client never attached"; cat "${R}/desktop.log"; exit 1; }

seq_no=0
desktop() { # desktop <command...>: run one control command, wait for its ack
  seq_no=$((seq_no + 1))
  printf '%s\n' "$*" > "${CTL}/cmd.${seq_no}.tmp" && mv "${CTL}/cmd.${seq_no}.tmp" "${CTL}/cmd.${seq_no}"
  for _ in $(seq 1 200); do
    [ -f "${CTL}/ack.${seq_no}" ] && return 0
    kill -0 "${desktop_pid}" 2>/dev/null || { echo "the desktop client died:"; tail -5 "${R}/desktop.log"; exit 1; }
    sleep 0.05
  done
  echo "desktop client did not answer: $*"; return 1
}

# 2. The TUI, in tmux. Ctrl+H focuses the sidebar; Chat, Issues, Activity,
#    then the workspaces: three rows down and Enter opens `dev`.
sb tmux -S "${TMUX_SOCK}" new-session -d -s tui -x "${TUI_W}" -y "${TUI_H}" \
  "cd '${REPO}' && SHELBI_ROOT='${R}' '${SHELBI_BIN}' --project ${PROJECT}"
sleep 4
tm send-keys -t tui C-h; sleep 0.4
tm send-keys -t tui j; sleep 0.2; tm send-keys -t tui j; sleep 0.2; tm send-keys -t tui j; sleep 0.2
tm send-keys -t tui Enter; sleep 2

# 3. `shelbi attach` from a terminal, in tmux.
sb tmux -S "${TMUX_SOCK}" new-session -d -s att -x "${ATTACH_W}" -y "${ATTACH_H}" \
  "SHELBI_ROOT='${R}' '${SHELBI_BIN}' --project ${PROJECT} attach dev"
sleep 2

capture() { tm capture-pane -t "$1" -p; }

# ---- checks -------------------------------------------------------------------------
# check.py <mode> <session WxH> <viewer WxH> <snapshot> <screen>
#   mode exact: the screen is the snapshot placed into the viewer, line for line
#   mode block: the placed snapshot appears as a block somewhere in the screen
cat > "${R}/check.py" <<'PY'
import sys, unicodedata
mode, session, viewer, snap_path, screen_path = sys.argv[1:6]
sc, sr = map(int, session.split("x"))
vc, vr = map(int, viewer.split("x"))

def cells(line):
    # One entry per terminal column: a wide character takes two (the second
    # empty), as tmux and the clients lay it out.
    out = []
    for ch in line:
        out.append(ch)
        if unicodedata.east_asian_width(ch) in "WF":
            out.append("")
    return out

def pad(row, width):
    return (row + [" "] * width)[:width]

snap = open(snap_path, encoding="utf-8", errors="replace").read().split("\n")
snap = [cells(l.rstrip()) for l in snap][:sr] + [[]] * max(0, sr - len(snap))
screen = [cells(l) for l in open(screen_path, encoding="utf-8", errors="replace").read().split("\n")]

def fit(s, v):  # (pad_before, len): letterbox centered, clip anchored at 0
    return ((v - s) // 2, s) if v >= s else (0, v)

(pc, lc), (pr, lr) = fit(sc, vc), fit(sr, vr)
want = []
for y in range(vr):
    r = y - pr
    row = ([" "] * pc + snap[r][:lc]) if 0 <= r < lr else []
    want.append(pad(row, vc))

def show(rows):
    return "\n".join("    |" + "".join(r).rstrip() for r in rows)

# `shelbi attach` shows its detach hint on the bottom row until the first key.
HINT = "detach: Ctrl+]"
if mode in ("exact", "exact-attach"):
    got = [pad(l, vc) for l in (screen + [[]] * vr)[:vr]]
    if mode == "exact-attach" and "".join(got[-1]).strip() == HINT:
        got[-1] = want[-1]
    if got != want:
        bad = next(i for i in range(vr) if got[i] != want[i])
        print(f"first difference at row {bad}\n  want:\n{show(want)}\n  got:\n{show(got)}")
        sys.exit(1)
else:
    # tmux trims trailing blanks; pad every row past where a block could end.
    width = max(len(l) for l in screen + [[]]) + vc
    rows = [pad(l, width) for l in screen]
    for y0 in range(len(rows) - vr + 1):
        for x0 in range(width - vc + 1):
            if all(rows[y0 + i][x0:x0 + vc] == want[i] for i in range(vr)):
                sys.exit(0)
    print(f"block not found\n  want:\n{show(want)}\n  screen:\n{show(rows)}")
    sys.exit(1)
PY

type_in() { # type_in <client> <text>
  case "$1" in
    desktop) desktop type "$2" && desktop key enter ;;
    tui) tm send-keys -t tui -l "$2"; tm send-keys -t tui Enter ;;
    attach) tm send-keys -t att -l "$2"; tm send-keys -t att Enter ;;
  esac
}

tui_size=""
round=0
for client in desktop tui attach desktop; do
  round=$((round + 1))
  marker="typed-in-${client}-${round}"
  type_in "${client}" "clear; echo ${marker}"
  # The client debounces its size (60 ms) and the session its resize (more);
  # give both, and bash's redraw, time to settle.
  sleep 1.5
  size="$(session_size)"
  case "${client}" in
    desktop) want_size="${DESKTOP_SIZE}" ;;
    attach) want_size="${ATTACH_W}x${ATTACH_H}" ;;
    tui) want_size="${size}"; tui_size="${size}" ;;   # the TUI's main area, learned here
  esac
  if [ "${size}" = "${want_size}" ]; then
    ok "round ${round} (${client} typed): the session follows it to ${size}"
  else
    fail "round ${round} (${client} typed): session is ${size}, want ${want_size}"
  fi
  [ "${client}" = tui ] && { [ "${size}" != "${DESKTOP_SIZE}" ] && [ "${size}" != "${ATTACH_W}x${ATTACH_H}" ] \
    || fail "round ${round}: the TUI's main area should differ from the other two (${size})"; }

  snapshot > "${R}/snap.txt"
  grep -q "^${marker}$" "${R}/snap.txt" || fail "round ${round}: the session never printed ${marker}"

  # The desktop app: its grid is the session's, and what it paints is the
  # session placed into its viewport.
  desktop dump "${R}/desktop-grid.txt" "${R}/desktop-view.txt"
  if diff <(sed 's/ *$//' "${R}/snap.txt" | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}') \
          <(sed 's/ *$//' "${R}/desktop-grid.txt" | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}') >"${R}/d.diff"; then
    ok "round ${round}: the desktop app's grid is the session's"
  else
    fail "round ${round}: the desktop app's grid differs from the session's"; cat "${R}/d.diff"
  fi
  if python3 "${R}/check.py" exact "${size}" "${DESKTOP_SIZE}" "${R}/snap.txt" "${R}/desktop-view.txt"; then
    ok "round ${round}: the desktop app paints it placed into ${DESKTOP_SIZE}"
  else
    fail "round ${round}: the desktop app's viewport is wrong"
  fi

  capture att > "${R}/attach.txt"
  if python3 "${R}/check.py" exact-attach "${size}" "${ATTACH_W}x${ATTACH_H}" "${R}/snap.txt" "${R}/attach.txt"; then
    ok "round ${round}: shelbi attach shows it placed into ${ATTACH_W}x${ATTACH_H}"
  else
    fail "round ${round}: shelbi attach shows something else"
  fi

  if [ -n "${tui_size}" ]; then
    capture tui > "${R}/tui.txt"
    if python3 "${R}/check.py" block "${size}" "${tui_size}" "${R}/snap.txt" "${R}/tui.txt"; then
      ok "round ${round}: the TUI shows it placed into its ${tui_size} main area"
    else
      fail "round ${round}: the TUI's main area shows something else"
    fi
  else
    capture tui > "${R}/tui.txt"
    grep -q "${marker}" "${R}/tui.txt" && ok "round ${round}: the TUI shows ${marker}" \
      || fail "round ${round}: the TUI doesn't show ${marker}"
  fi
done

desktop quit >/dev/null 2>&1
echo
if [ "${failures}" -eq 0 ]; then
  echo "multiclient-check: all checks passed"
else
  echo "multiclient-check: ${failures} check(s) failed"
fi
exit $(( failures > 0 ))
