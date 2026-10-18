#!/usr/bin/env bash
# The real-window half of the Figma pixel QA: the bundled Shelbi.app in a
# real 1440x1024 window, over the same mockup content `--visual-figma`
# renders offscreen, walked through every screen in each theme (Auto, then
# Dark, then Light, switched live in Settings), captured, and diffed against
# John's reference PNGs (and, with --offscreen, against the offscreen frames).
#
#   desktop/scripts/figma-window-walk.sh [options]
#
#   --app PATH        Shelbi.app to walk (default: desktop/target/bundle/
#                     Shelbi.app; build it with scripts/bundle.sh)
#   --cli PATH        the shelbi CLI the app talks to (default: `shelbi` on
#                     PATH). It must speak the app's control protocol.
#   --out DIR         where captures and diffs go (default: a new
#                     ~/.shelbi/projects/shelbi/design-refs/desktop-figma/
#                     qa/window-walk-<time>/)
#   --offscreen DIR   also diff each capture against `--visual-figma DIR`'s
#                     frame of the same name
#   --keep            keep the sandbox root afterwards (it is printed)
#
# It needs an unlocked screen, Screen Recording and Accessibility for the
# terminal it runs in, a main display at least 1440x1064 points, ImageMagick
# 7 and the Xcode command line tools (swiftc). It takes the keyboard and
# mouse for a couple of minutes: it posts keys to the app's process and
# clicks (Settings' theme segments, the font field) at the window's points,
# putting the pointer back afterwards. Don't type while it runs.
#
# Safety: everything runs against a fresh Shelbi root in /tmp (SHELBI_ROOT
# and SHELBI_HOME), with the hub and control socket variables unset, so the
# real hub is never touched. `claude` and `codex` on the sandbox's PATH are a
# fake agent that prints a transcript and waits, so no real agent starts. The
# only process it stops is the sandbox daemon, by the pid in the sandbox's
# own pid file, and the app it launched.
#
# What it captures, in <out>/<theme>/ (theme = auto, dark, light):
#   <screen>-<appearance>.png   named as the references are (orchestrator,
#                               issues, issue-detail, activity, review,
#                               workspace, workspace-idle, machines, settings,
#                               command-palette); <appearance> is the theme
#                               the window resolved to (Auto: the system's)
#   diff/                       figma-diff.sh against the references
#   offscreen-diff/diff/        against --offscreen's frames, when given
# and once, in <out>/fonts/: the font picker open, the font picked (and the
# saved `terminal_font_family`), the terminal zoomed with ⌘= (and the saved
# `terminal_font_size`), then ⌘0. <out>/walk.log records each step.
#
# The terminal screens show the fake agent's transcript, not the mockups',
# and remote probes run for real (the mockup's `gpu.lan` reads unreachable),
# so those regions differ by content; the chrome around them is the check.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # desktop/
app="$here/target/bundle/Shelbi.app"
cli="$(command -v shelbi || true)"
out=""
offscreen=""
keep=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) app="$2"; shift 2 ;;
    --cli) cli="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --offscreen) offscreen="$2"; shift 2 ;;
    --keep) keep=1; shift ;;
    -h|--help) sed -n '2,52p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "figma-window-walk.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

fail() { printf 'figma-window-walk.sh: %s\n' "$*" >&2; exit 1; }
[[ "$(uname -s)" == Darwin ]] || fail "macOS only"
[[ -x "$app/Contents/MacOS/Shelbi" ]] || fail "no app at $app (run scripts/bundle.sh, or pass --app)"
[[ -n "$cli" && -x "$cli" ]] || fail "no shelbi CLI (put it on PATH or pass --cli)"
command -v magick >/dev/null || fail "needs ImageMagick 7 (brew install imagemagick)"
command -v swiftc >/dev/null || fail "needs swiftc (xcode-select --install)"
refs="$HOME/.shelbi/projects/shelbi/design-refs/desktop-figma"
[[ -n "$out" ]] || out="$refs/qa/window-walk-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
log="$out/walk.log"
say() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$log"; }

# ---- the helper ---------------------------------------------------------------
root="$(mktemp -d /tmp/shfw.XXXXXX)"
helper="$root/walk-helper"
cat > "$root/walk-helper.swift" <<'SWIFT'
// walk-helper <cmd> ...: the window walk's hands.
//   locked | display                    is the screen locked; the main display's size
//   window <pid>                        "<id> <x> <y> <w> <h>" of its main window
//   press <pid> "Menu>Item"             press a menu item (the app activated first)
//   key <pid> <keycode> [cmd,shift,opt,ctrl]   a key down/up posted to the pid
//   type <pid> <text>                   each character posted to the pid
//   click <pid> <x> <y>                 activate, click at a global point, put
//                                       the pointer back
import AppKit
import ApplicationServices
let a = CommandLine.arguments
func attr(_ e: AXUIElement, _ n: String) -> AnyObject? { var v: AnyObject?; AXUIElementCopyAttributeValue(e, n as CFString, &v); return v }
func activate(_ pid: pid_t) { NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows]); usleep(300_000) }
func post(_ pid: pid_t, _ code: CGKeyCode, _ flags: CGEventFlags, _ text: String? = nil) {
  for down in [true, false] {
    let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!
    e.flags = flags
    if let t = text { let u = Array(t.utf16); e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u) }
    e.postToPid(pid); usleep(40_000)
  }
}
switch a[1] {
case "locked":
  let d = CGSessionCopyCurrentDictionary() as? [String: Any]
  print((d?["CGSSessionScreenIsLocked"] as? Bool) == true ? "locked" : "unlocked")
case "display":
  let f = NSScreen.screens[0].visibleFrame; print(Int(f.width), Int(f.height))
case "window":
  let pid = Int32(a[2])!
  let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
  for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid && (w[kCGWindowLayer as String] as? Int) == 0 {
    let b = w[kCGWindowBounds as String] as! [String: CGFloat]
    if b["Width"]! < 100 { continue }
    print(w[kCGWindowNumber as String]!, Int(b["X"]!), Int(b["Y"]!), Int(b["Width"]!), Int(b["Height"]!)); break
  }
case "press":
  let pid = Int32(a[2])!; activate(pid)
  var el: AXUIElement = attr(AXUIElementCreateApplication(pid), kAXMenuBarAttribute) as! AXUIElement
  for name in a[3].split(separator: ">") {
    var kids = attr(el, kAXChildrenAttribute) as? [AXUIElement] ?? []
    if kids.count == 1, (attr(kids[0], kAXRoleAttribute) as? String) == "AXMenu" { kids = attr(kids[0], kAXChildrenAttribute) as? [AXUIElement] ?? [] }
    guard let k = kids.first(where: { (attr($0, kAXTitleAttribute) as? String) == String(name) }) else { print("no menu item \(name)"); exit(1) }
    el = k
  }
  exit(AXUIElementPerformAction(el, kAXPressAction as CFString) == .success ? 0 : 1)
case "key":
  var flags: CGEventFlags = []
  if a.count > 4 { for m in a[4].split(separator: ",") { switch m { case "cmd": flags.insert(.maskCommand); case "shift": flags.insert(.maskShift); case "opt": flags.insert(.maskAlternate); case "ctrl": flags.insert(.maskControl); default: break } } }
  post(Int32(a[2])!, CGKeyCode(a[3])!, flags)
case "type":
  for ch in a[3] { post(Int32(a[2])!, 0, [], String(ch)) }
case "click":
  let pid = Int32(a[2])!; activate(pid)
  let back = CGEvent(source: nil)!.location
  let p = CGPoint(x: Double(a[3])!, y: Double(a[4])!)
  for t in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
    CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!.post(tap: .cghidEventTap); usleep(60_000)
  }
  CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: back, mouseButton: .left)!.post(tap: .cghidEventTap)
default: exit(2)
}
SWIFT
swiftc -O "$root/walk-helper.swift" -o "$helper" 2>>"$log" || fail "could not build the helper (see $log)"

[[ "$("$helper" locked)" == unlocked ]] || fail "the screen is locked: a locked session draws no frames. Unlock it and run again."
read -r dw dh < <("$helper" display)
(( dw >= 1440 && dh >= 1064 )) || fail "the main display is ${dw}x${dh} points; the walk needs 1440x1064 for a 1440x1024 window"

# ---- the sandbox --------------------------------------------------------------
say "sandbox root $root"
mkdir -p "$root/bin"
cat > "$root/bin/fake-agent" <<'AGENT'
#!/bin/sh
# Stands in for claude and codex: a short transcript, then waits.
case "$1" in --version|-v) echo "0.0.0 (figma-window-walk fake)"; exit 0 ;; esac
printf '\033]2;shelbi:idle\007'
printf '\033[1m>_ Fake agent\033[0m (figma-window-walk)\n\n\033[2mdirectory: %s\033[0m\n\n' "$PWD"
i=1; while [ $i -le 60 ]; do printf '  line %02d of the transcript, enough to scroll\n' $i; i=$((i+1)); done
printf '\n\033[1m›\033[0m Ask Shelbi to do anything\n'
exec cat >/dev/null
AGENT
chmod +x "$root/bin/fake-agent"
ln -s fake-agent "$root/bin/claude"
ln -s fake-agent "$root/bin/codex"
sb() { # exec, so a backgrounded `sb app &` leaves $! the app's own pid
  exec env -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_HUB_ADDR -u SHELBI_PROJECT \
    -u SHELBI_MANAGED_CONTEXT -u SHELBI_AGENT -u SHELBI_BIN -u TASK_ID -u PROJECT \
    SHELBI_ROOT="$root/home" SHELBI_HOME="$root/home" PATH="$root/bin:$(dirname "$cli"):/usr/bin:/bin:/usr/sbin:/sbin" "$@"
}
app_pid=""
cleanup() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then kill "$app_pid" 2>/dev/null || true; fi
  # The sandbox daemon, by its own pid file ("<pid> <started> <version>"),
  # then the sandbox's sessions (their command lines carry the sandbox
  # path) and the fake agents under them. Nothing outside $root.
  local daemon pid
  daemon="$(cut -d' ' -f1 "$root/home/shelbi.pid" 2>/dev/null || true)"
  if [[ -n "$daemon" ]] && ps -o command= -p "$daemon" 2>/dev/null | grep -q 'shelbi daemon'; then kill "$daemon" 2>/dev/null || true; fi
  for pid in $(pgrep -f -- "$root/" || true); do
    [[ "$pid" == "$$" ]] && continue
    ps -o command= -p "$pid" 2>/dev/null | grep -q -- "__session.*$root/" || continue
    pkill -P "$pid" 2>/dev/null || true
    kill "$pid" 2>/dev/null || true
  done
  if (( keep )); then say "kept the sandbox root $root"; else rm -rf "$root"; fi
}
trap cleanup EXIT

say "writing the mockup fixtures (cargo run --features visual-check -- --visual-figma-fixtures)"
(cd "$here" && env -u SHELBI_HUB_SOCK -u SHELBI_CONTROL_SOCK -u SHELBI_HUB_ADDR \
  cargo run -q --features visual-check -- --visual-figma-fixtures "$root/home") >>"$log" 2>&1 \
  || fail "the fixtures export failed (see $log)"
git -C "$root/home" init -q work 2>/dev/null || mkdir -p "$root/home/work"
printf 'theme: auto\ndesktop:\n  cli_path: %s\n' "$cli" > "$root/home/config.yaml"
cat > "$root/home/desktop-windows.json" <<JSON
{"slots": [{"project": "my-project", "open": true,
  "frame": {"x": 100.0, "y": 40.0, "width": 1440.0, "height": 1024.0},
  "sidebar_width": 28, "sidebar_collapsed": false}]}
JSON

# ---- launch -------------------------------------------------------------------
say "launching $app"
cd "$root/home/work"
sb "$app/Contents/MacOS/Shelbi" my-project >>"$out/app.log" 2>&1 &
app_pid=$!
cd "$here"
win=""
for _ in $(seq 1 60); do
  win="$("$helper" window "$app_pid" || true)"
  [[ -n "$win" ]] && break
  sleep 0.5
done
[[ -n "$win" ]] || fail "the app showed no window (see $out/app.log)"
read -r wid wx wy ww wh <<<"$win"
say "window $wid at $wx,$wy ${ww}x${wh}"
[[ "$ww" == 1440 && "$wh" == 1024 ]] || say "warning: the window is ${ww}x${wh}, not 1440x1024"
sleep 4 # the daemon starts, the sidebar and views load

# US key codes.
K_O=31; K_I=34; K_L=37; K_P=35; K_COMMA=43; K_RETURN=36; K_ESC=53
K_1=18; K_5=23; K_6=22; K_EQUAL=24; K_0=29
key() { "$helper" key "$app_pid" "$@"; sleep 0.25; }
settle() { sleep "${1:-1.5}"; }
click() { "$helper" click "$app_pid" $((wx + $1)) $((wy + $2)); settle 0.8; }
capture() { # capture <dir> <name>
  mkdir -p "$1"
  screencapture -x -o -l"$wid" "$1/$2.png" || fail "screencapture failed: give the terminal Screen Recording"
  say "  captured $1/$2.png"
}
config_value() { sed -n "s/^ *$1: *//p" "$root/home/config.yaml" | head -1; }

# Settings' theme segments, in window points (Settings at the main area's
# top: Auto, Dark, Light at x 339 / 398 / 458, y 119).
theme_x() { case "$1" in auto) echo 339 ;; dark) echo 398 ;; light) echo 458 ;; esac; }
appearance() { # the theme the window resolves to
  case "$1" in
    auto) [[ "$(defaults read -g AppleInterfaceStyle 2>/dev/null || true)" == Dark ]] && echo dark || echo light ;;
    *) echo "$1" ;;
  esac
}

walk() { # walk <theme>
  local theme="$1" dir="$out/$1" look
  look="$(appearance "$theme")"
  say "theme $theme (draws $look)"
  key $K_COMMA cmd; settle
  click "$(theme_x "$theme")" 119; settle
  capture "$dir" "settings-$look"
  key $K_O cmd; settle; capture "$dir" "orchestrator-$look"
  key $K_P ctrl; settle; capture "$dir" "command-palette-$look"; key $K_ESC; settle 0.6
  key $K_I cmd; settle; capture "$dir" "issues-$look"
  # The Review column's card (h/l move between columns), then open it.
  "$helper" type "$app_pid" "lll"; settle 0.6; key $K_RETURN; settle
  capture "$dir" "issue-detail-$look"; key $K_ESC; settle 0.6
  key $K_L cmd; settle; capture "$dir" "activity-$look"
  key $K_P ctrl; settle 0.6; "$helper" type "$app_pid" "machines"; settle 0.6; key $K_RETURN; settle
  capture "$dir" "machines-$look"
  key $K_1 cmd; settle 3; capture "$dir" "review-$look"
  key $K_5 cmd; settle 3; capture "$dir" "workspace-$look"
  key $K_6 cmd; settle 2; capture "$dir" "workspace-idle-$look"
  key $K_O cmd; settle
}

for theme in auto dark light; do walk "$theme"; done

# ---- fonts --------------------------------------------------------------------
say "fonts"
key $K_COMMA cmd; settle
click 491 213; settle 1; capture "$out/fonts" picker-open
"$helper" type "$app_pid" "Geist Mono"; settle 0.6; key $K_RETURN; settle
capture "$out/fonts" picker-picked
say "  terminal_font_family: $(config_value terminal_font_family)"
key $K_O cmd; settle
key $K_EQUAL cmd; key $K_EQUAL cmd; settle
capture "$out/fonts" zoomed
say "  terminal_font_size after ⌘= twice: $(config_value terminal_font_size)"
key $K_0 cmd; settle
say "  terminal_font_size after ⌘0: $(config_value terminal_font_size || true) (blank = the default)"

# ---- diff ---------------------------------------------------------------------
for theme in auto dark light; do
  say "diff $theme against the references"
  "$here/scripts/figma-diff.sh" "$out/$theme" "$refs" | tee -a "$log"
  if [[ -n "$offscreen" ]]; then
    say "diff $theme against the offscreen frames in $offscreen"
    mkdir -p "$out/$theme/offscreen-diff"
    cp "$out/$theme"/*.png "$out/$theme/offscreen-diff/"
    "$here/scripts/figma-diff.sh" "$out/$theme/offscreen-diff" "$offscreen" | tee -a "$log"
  fi
done
say "done: $out"
