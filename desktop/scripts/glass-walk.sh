#!/usr/bin/env bash
# The translucent window in a real window: the glass and the fills on it
# (buttons, filter chips, the search box, cards, the task pane over the
# board, Settings' fields) over a striped backdrop window, captured at
# 85% / blur 30 (the new-install default), 30% / blur 50, 30% / blur 0 and
# 100%, in each theme.
#
#   desktop/scripts/glass-walk.sh [--bin PATH] [--cli PATH] [--out DIR] [--theme dark|light]
#
#   --bin PATH   the desktop binary (default: desktop/target/release/shelbi-desktop;
#                build it with `cargo build --release --features visual-check`,
#                the fixtures come from its --visual-figma-fixtures)
#   --cli PATH   a shelbi CLI of the app's version (default: target/debug/shelbi
#                at the repo root; `cargo build -p shelbi`)
#   --out DIR    where the captures go (default: a new /tmp/glass-walk.XXXXXX)
#   --theme T    dark (default) or light
#
# What to look for, in <out>/o<opacity>-b<blur>/{issues,task-pane,settings}.png:
# at blur 0 the backdrop's 6px stripes run sharp under the glass AND under
# every fill (the fills composite over the same desktop, no separate pass);
# at blur 30 and 50 they wash out evenly under both (the window server's
# one background blur, at the setting's radius, never doubled or missing).
# The task pane shows only the glass (the board is clipped under it); at
# 100% the window is opaque.
#
# Needs an unlocked screen, Screen Recording for the terminal it runs in,
# and swiftc. It posts keys to the app it launched and captures only the
# window's rectangle, over its own backdrop window, never other apps.
#
# Safety: a fresh Shelbi root in /tmp (SHELBI_ROOT and SHELBI_HOME) with the
# hub and control socket variables unset, so the real hub is never touched;
# `claude` and `codex` are a fake agent. It stops only the sandbox daemon
# (by the sandbox's own pid file), the sandbox's sessions, the app and the
# backdrop it launched.
set -uo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)" # desktop/
bin="$here/target/release/shelbi-desktop"
cli="$here/../target/debug/shelbi"
out=""
theme=dark
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) bin="$2"; shift 2 ;;
    --cli) cli="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --theme) theme="$2"; shift 2 ;;
    -h|--help) sed -n '2,35p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "glass-walk.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -x "$bin" ]] || { echo "glass-walk.sh: no desktop binary at $bin" >&2; exit 1; }
[[ -x "$cli" ]] || { echo "glass-walk.sh: no shelbi CLI at $cli" >&2; exit 1; }
[[ -n "$out" ]] || out="$(mktemp -d /tmp/glass-walk.XXXXXX)"
mkdir -p "$out"; log="$out/walk.log"
say() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$log"; }
tools="$(mktemp -d /tmp/glass-tools.XXXXXX)"
helper="$tools/helper"
cat > "$tools/helper.swift" <<'SWIFT'
import AppKit
import ApplicationServices
let a = CommandLine.arguments
func activate(_ pid: pid_t) { NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows]); usleep(300_000) }
func post(_ pid: pid_t, _ code: CGKeyCode, _ flags: CGEventFlags, _ text: String? = nil) {
  for down in [true, false] {
    let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!
    e.flags = flags
    if let t = text { let u = Array(t.utf16); e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u) }
    e.postToPid(pid); usleep(40_000)
  }
}
final class Stripes: NSView {
  override func draw(_ r: NSRect) {
    NSColor.white.setFill(); bounds.fill()
    NSColor.black.setFill()
    var x: CGFloat = 0
    while x < bounds.width { NSRect(x: x, y: 0, width: 6, height: bounds.height).fill(); x += 12 }
    let colors: [NSColor] = [.systemRed, .systemYellow, .systemBlue, .systemGreen]
    var y: CGFloat = 0; var i = 0
    while y < bounds.height { colors[i % 4].setFill(); NSRect(x: 0, y: y, width: bounds.width, height: 40).fill(); y += 160; i += 1 }
    let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.boldSystemFont(ofSize: 64), .foregroundColor: NSColor.magenta]
    var ty: CGFloat = 60
    while ty < bounds.height { ("BACKDROP " as NSString).draw(at: NSPoint(x: 40, y: ty), withAttributes: attrs); ty += 220 }
  }
}
switch a[1] {
case "locked":
  let d = CGSessionCopyCurrentDictionary() as? [String: Any]
  print((d?["CGSSessionScreenIsLocked"] as? Bool) == true ? "locked" : "unlocked")
case "display":
  let f = NSScreen.screens[0].frame; let v = NSScreen.screens[0].visibleFrame; print(Int(f.width), Int(f.height), Int(v.origin.y))
case "window":
  let pid = Int32(a[2])!
  let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
  for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid && (w[kCGWindowLayer as String] as? Int) == 0 {
    let b = w[kCGWindowBounds as String] as! [String: CGFloat]
    if b["Width"]! < 300 || b["Height"]! < 300 { continue }
    print(w[kCGWindowNumber as String]!, Int(b["X"]!), Int(b["Y"]!), Int(b["Width"]!), Int(b["Height"]!))
  }
case "key":
  var flags: CGEventFlags = []
  if a.count > 4 { for m in a[4].split(separator: ",") { switch m { case "cmd": flags.insert(.maskCommand); case "shift": flags.insert(.maskShift); case "opt": flags.insert(.maskAlternate); case "ctrl": flags.insert(.maskControl); default: break } } }
  post(Int32(a[2])!, CGKeyCode(a[3])!, flags)
case "type":
  for ch in a[3] { post(Int32(a[2])!, 0, [], String(ch)) }
case "activate":
  activate(Int32(a[2])!)
case "backdrop":
  // backdrop x y w h (top-left screen points): a striped window to see the blur on.
  let app = NSApplication.shared
  app.setActivationPolicy(.accessory)
  let screenH = NSScreen.screens[0].frame.height
  let x = CGFloat(Double(a[2])!), y = CGFloat(Double(a[3])!), w = CGFloat(Double(a[4])!), h = CGFloat(Double(a[5])!)
  let win = NSWindow(contentRect: NSRect(x: x, y: screenH - y - h, width: w, height: h), styleMask: [.borderless], backing: .buffered, defer: false)
  win.contentView = Stripes(frame: NSRect(x: 0, y: 0, width: w, height: h))
  win.orderFrontRegardless()
  signal(SIGUSR1, SIG_IGN)
  let src = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
  src.setEventHandler { win.level = .floating; win.orderFrontRegardless(); win.level = .normal; win.orderFrontRegardless() }
  src.resume()
  app.run()
default: exit(2)
}
SWIFT
swiftc -O "$tools/helper.swift" -o "$helper" 2>>"$log" || { say "could not build the helper (see $log)"; exit 1; }
[[ "$("$helper" locked)" == unlocked ]] || { say "the screen is locked: unlock it and run again"; exit 1; }
root="$(mktemp -d /tmp/shbw.XXXXXX)"
say "sandbox $root"
mkdir -p "$root/bin"
cat > "$root/bin/fake-agent" <<'AGENT'
#!/bin/sh
case "$1" in --version|-v) echo "0.0.0 (fake)"; exit 0 ;; esac
printf '\033]2;shelbi:idle\007'
i=1; while [ $i -le 40 ]; do printf '  line %02d of the transcript\n' $i; i=$((i+1)); done
exec cat >/dev/null
AGENT
chmod +x "$root/bin/fake-agent"; ln -s fake-agent "$root/bin/claude"; ln -s fake-agent "$root/bin/codex"
sb() {
  exec env -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_HUB_ADDR -u SHELBI_PROJECT \
    -u SHELBI_MANAGED_CONTEXT -u SHELBI_AGENT -u SHELBI_BIN -u TASK_ID -u PROJECT \
    SHELBI_ROOT="$root/home" SHELBI_HOME="$root/home" PATH="$root/bin:$(dirname "$cli"):/usr/bin:/bin:/usr/sbin:/sbin" "$@"
}
app_pid=""; back_pid=""
cleanup() {
  [[ -n "$app_pid" ]] && kill "$app_pid" 2>/dev/null
  [[ -n "$back_pid" ]] && kill "$back_pid" 2>/dev/null
  local daemon pid
  daemon="$(cut -d' ' -f1 "$root/home/shelbi.pid" 2>/dev/null || true)"
  if [[ -n "$daemon" ]] && ps -o command= -p "$daemon" 2>/dev/null | grep -q 'shelbi daemon'; then kill "$daemon" 2>/dev/null; fi
  for pid in $(pgrep -f -- "$root/" || true); do
    [[ "$pid" == "$$" ]] && continue
    ps -o command= -p "$pid" 2>/dev/null | grep -q -- "__session.*$root/" || continue
    pkill -P "$pid" 2>/dev/null; kill "$pid" 2>/dev/null
  done
  rm -rf "$root" "$tools"
}
trap cleanup EXIT
(env -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_HUB_ADDR "$bin" --visual-figma-fixtures "$root/home") >>"$log" 2>&1 || { say "fixtures failed"; exit 1; }
git -C "$root/home" init -q work 2>/dev/null || mkdir -p "$root/home/work"
X=120; Y=80; W=1280; H=860
"$helper" backdrop $((X-40)) $((Y-40)) $((W+80)) $((H+80)) & back_pid=$!
sleep 1
K_I=34; K_COMMA=43; K_RETURN=36; K_ESC=53
run() { # run <opacity> <blur>
  local op="$1" bl="$2" dir="$out/o$1-b$2"; mkdir -p "$dir"
  printf 'theme: %s\ndesktop:\n  cli_path: %s\n  window_opacity: %s\n  window_blur: %s\n' "$theme" "$cli" "$op" "$bl" > "$root/home/config.yaml"
  cat > "$root/home/desktop-windows.json" <<JSON
{"slots": [{"project": "my-project", "open": true,
  "frame": {"x": $X.0, "y": $Y.0, "width": $W.0, "height": $H.0},
  "sidebar_width": 28, "sidebar_collapsed": false}]}
JSON
  local d; d="$(cut -d' ' -f1 "$root/home/shelbi.pid" 2>/dev/null)"
  if [[ -z "$d" ]] || ! kill -0 "$d" 2>/dev/null; then
    ( cd "$root/home/work" && sb "$cli" daemon run ) >>"$out/daemon.log" 2>&1 &
    sleep 3; say "sandbox daemon: $(cat "$root/home/shelbi.pid" 2>/dev/null)"
  fi
  kill -USR1 "$back_pid"; sleep 0.5
  cd "$root/home/work"
  sb "$bin" my-project >>"$out/app.log" 2>&1 &
  app_pid=$!
  cd "$out"
  local win=""
  for _ in $(seq 1 50); do win="$("$helper" window "$app_pid" | grep " $W $H$" | head -1 || true)"; [[ -n "$win" ]] && break; sleep 0.5; done
  [[ -n "$win" ]] || { say "no window: $("$helper" window "$app_pid")"; cp -R "$root/home" "$out/home-copy" 2>/dev/null; kill "$app_pid"; app_pid=""; return 1; }
  read -r wid wx wy ww wh <<<"$win"; say "window $wid $wx,$wy ${ww}x${wh} (opacity $op blur $bl)"
  sleep 5
  "$helper" activate "$app_pid"
  key() { "$helper" key "$app_pid" "$@"; sleep 0.3; }
  cap() { sleep 1.5; screencapture -x -R"$wx,$wy,$ww,$wh" "$dir/$1.png" && say "  $dir/$1.png"; }
  key $K_I cmd; cap issues
  key $K_RETURN; cap task-pane
  key $K_ESC; sleep 0.5
  key $K_COMMA cmd; cap settings
  kill "$app_pid" 2>/dev/null; wait "$app_pid" 2>/dev/null; app_pid=""
  sleep 1
}
for r in ${RUNS:-85:30 30:50 30:0 100:0}; do run ${r%%:*} ${r##*:}; done
say "done: $out"
