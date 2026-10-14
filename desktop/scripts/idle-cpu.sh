#!/usr/bin/env bash
# Idle CPU of the real app with one attached session.
#
# Usage: desktop/scripts/idle-cpu.sh [settle-seconds] [measure-seconds]
#   $SHELBI_BIN    the `shelbi` CLI (default: `shelbi` on PATH)
#   $DESKTOP_BIN   the app (default: desktop/target/release/shelbi-desktop)
#
# Launches the full app (window, sidebar, refresh pipeline, control watch) on
# a sandboxed project whose orchestrator "agent" is `sleep`, so its Chat view
# is attached to a live, quiet session. After the settle time (default 20 s:
# the cursor blinks for 10 s after the last activity, then stops) it reads the
# app's CPU time twice, `measure` seconds apart (default 20), and counts the
# frames the terminal painted in that window (its per-frame trace events).
# Prints CPU as a share of one core.
#
# The app activates its window (it takes focus) for the run. Everything runs
# in a sandboxed root under /tmp with the hub's socket variables unset;
# teardown signals only the app's pid and pids from the sandbox's own files.

set -uo pipefail

SETTLE="${1:-20}"
MEASURE="${2:-20}"
here="$(cd "$(dirname "$0")" && pwd)"
SHELBI_BIN="${SHELBI_BIN:-$(command -v shelbi)}"
DESKTOP_BIN="${DESKTOP_BIN:-${here}/../target/release/shelbi-desktop}"
[ -x "${SHELBI_BIN}" ] || { echo "no shelbi CLI (set SHELBI_BIN)"; exit 2; }
[ -x "${DESKTOP_BIN}" ] || { echo "no ${DESKTOP_BIN}: cargo build --release"; exit 2; }

R="$(mktemp -d /tmp/shi.XXXX)"
PROJECT=idle
REPO="${R}/repo"
app_pid=""

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

cleanup() {
  pids="${app_pid}"
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

mkdir -p "${R}/projects" "${REPO}"
git -C "${REPO}" init -q -b main
git -C "${REPO}" -c user.email=idle@shelbi.local -c user.name=idle commit -q --allow-empty -m init
cat > "${R}/projects/${PROJECT}.yaml" <<YAML
name: ${PROJECT}
repo: ${REPO}
default_branch: main
orchestrator: { runner: noop }
agent_runners:
  noop: { command: sleep, flags: ["1000000"] }
machines:
  - { name: local, kind: local, work_dir: ${REPO} }
workspaces:
  - { name: dev, machine: local }
YAML
cat > "${R}/config.yaml" <<YAML
desktop:
  cli_path: ${SHELBI_BIN}
YAML

# `sb_bg`, not `sb ... &`: backgrounding a shell function forks a subshell, and
# `$!` would be that subshell's pid rather than the app's.
RUST_LOG="warn,shelbi_desktop::terminal=debug,shelbi_desktop::terminal::frame=trace" \
  sb_bg "${R}/app.log" "${DESKTOP_BIN}" "${PROJECT}"
app_pid="${bg_pid}"
sleep "${SETTLE}"
kill -0 "${app_pid}" 2>/dev/null || { echo "the app exited:"; tail -20 "${R}/app.log"; exit 1; }
if ! grep -q " attached" "${R}/app.log"; then
  echo "the Chat view's terminal never attached; the app's log:"
  tail -30 "${R}/app.log"
  sb "${SHELBI_BIN}" session ls 2>&1 | tail -5
  exit 1
fi

cpu_seconds() { # ps TIME as [[dd-]hh:]mm:ss.ss -> seconds
  ps -o time= -p "${app_pid}" | awk -F'[:-]' '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; print s }'
}
frames() { grep -c "terminal::frame" "${R}/app.log"; }
lines_before=$(frames)
t0=$(cpu_seconds); start=$(python3 -c 'import time; print(time.time())')
sleep "${MEASURE}"
t1=$(cpu_seconds); end=$(python3 -c 'import time; print(time.time())')
lines_after=$(frames)
sessions=$(sb "${SHELBI_BIN}" session ls 2>/dev/null | awk '$3=="live"' | wc -l | tr -d ' ')

python3 - "$t0" "$t1" "$start" "$end" <<'PY'
import sys
t0, t1, a, b = map(float, sys.argv[1:5])
print(f"idle CPU: {100 * (t1 - t0) / (b - a):.2f}% of a core over {b - a:.1f} s")
PY
echo "terminal frames painted while idle: $((lines_after - lines_before)) (in all since launch: ${lines_after})"
echo "live sessions in the sandbox: ${sessions}"
