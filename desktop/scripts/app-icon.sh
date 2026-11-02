#!/usr/bin/env bash
# Regenerate everything cut from the app icons' sources (see
# assets/icon/README.md). macOS with Xcode 26 or later (actool and Icon
# Composer's .icon format); run from anywhere:
#
#   desktop/scripts/app-icon.sh
#
# Command Grille, from its layered Icon Composer document
# (assets/icon/Shelbi.icon):
#
#   assets/icon/compiled/Assets.car     the layered icon macOS 26 draws, plus
#                                       flattened renditions for macOS 15
#   assets/icon/compiled/Shelbi.icns    actool's flattened fallback
#   assets/icon/compiled/Shelbi.icon.sha256
#                                       the source these were compiled from
#                                       (bundle.sh refuses a stale compile)
#   assets/icon/command-grille-{512,224,128}.png
#                                       the compiled icon as macOS draws it
#                                       (mask, glass, shadow), for the app's
#                                       own use: the Dock, About, Settings
#
# Racing S, from its layered Icon Composer document
# (assets/icon/RacingS.icon), compiled the same way into a scratch directory
# (the bundle never wears it, so its compile isn't kept):
#
#   assets/icon/racing-s-{512,224,128}.png
#                                       drawn as macOS draws it, like Command
#                                       Grille's
#
# Retro, from its 1024px master (assets/icon/retro-1024.png):
#
#   assets/icon/retro-{512,224,128}.png
#
# Retro Racing S, from its 1024px master (assets/icon/retro-racing-s-1024.png):
#
#   assets/icon/retro-racing-s-{512,224,128}.png
#
# and the TUI About tables (crates/shelbi-tui/src/overlay/about_icon.rs).
#
# bundle.sh only copies the compiled files, so a release never needs Xcode
# 26; commit what this script writes.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # desktop/
icon_dir="$here/assets/icon"
compiled="$icon_dir/compiled"

fail() {
  printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || fail "app-icon.sh needs macOS"
actool_version="$(xcrun actool --version 2>/dev/null |
  /usr/bin/python3 -c 'import plistlib,sys; print(plistlib.loads(sys.stdin.buffer.read())["com.apple.actool.version"]["short-bundle-version"])' ||
  true)"
[[ "${actool_version%%.*}" =~ ^[0-9]+$ && "${actool_version%%.*}" -ge 26 ]] ||
  fail "needs Xcode 26 or later for actool to compile a .icon (found actool '${actool_version:-none}'; xcode-select -s /Applications/Xcode.app)"

work="$(mktemp -d)"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
registered=()
cleanup() {
  for app in "${registered[@]+"${registered[@]}"}"; do
    "$lsregister" -u "$app" >/dev/null 2>&1 || true
  done
  rm -rf "$work"
}
trap cleanup EXIT

# The digest bundle.sh checks: every file in the .icon, by path and content.
source_digest() {
  (cd "$icon_dir" && find Shelbi.icon -type f ! -name .DS_Store -print0 | LC_ALL=C sort -z |
    xargs -0 shasum -a 256) | shasum -a 256 | cut -c1-64
}

# compile_icon <Name> <out-dir>: actool <Name>.icon into <out-dir>
# (<Name>.icns and Assets.car).
compile_icon() {
  local name="$1" out="$2"
  echo "==> compiling $icon_dir/$name.icon (actool $actool_version)"
  mkdir -p "$out"
  xcrun actool "$icon_dir/$name.icon" \
    --compile "$out" \
    --app-icon "$name" \
    --output-partial-info-plist "$out/partial.plist" \
    --platform macosx \
    --minimum-deployment-target 15.0 \
    --include-all-app-icons >/dev/null
  [[ -s "$out/Assets.car" && -s "$out/$name.icns" ]] || fail "actool wrote no Assets.car / $name.icns for $name.icon"
}

# draw_icon <Name> <compiled-dir> <png-prefix>: draw a compiled icon as
# macOS shows it (mask, glass, shadow) into <png-prefix>-{512,224,128}.png.
# A throwaway app around the compiled icon (any executable will do; without
# one Finder badges the icon), registered so NSWorkspace draws its icon.
draw_icon() {
  local name="$1" from="$2" prefix="$3"
  local app="$work/render-$name/IconRender.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$from/Assets.car" "$from/$name.icns" "$app/Contents/Resources/"
  cp /usr/bin/true "$app/Contents/MacOS/IconRender"
  cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>IconRender</string>
	<key>CFBundleIdentifier</key>
	<string>dev.shelbi.icon-render.$name.$$</string>
	<key>CFBundleIconFile</key>
	<string>$name</string>
	<key>CFBundleIconName</key>
	<string>$name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
</dict>
</plist>
PLIST
  codesign --force --sign - "$app" >/dev/null 2>&1
  "$lsregister" -f "$app"
  registered+=("$app")
  for size in 512 224 128; do
    "$work/render-app-icon" "$app" "$size" "$prefix-$size.png"
  done
}

xcrun swiftc -O "$here/scripts/render-app-icon.swift" -o "$work/render-app-icon"

compile_icon Shelbi "$work/Shelbi"
mkdir -p "$compiled"
cp "$work/Shelbi/Assets.car" "$work/Shelbi/Shelbi.icns" "$compiled/"
source_digest >"$compiled/Shelbi.icon.sha256"
echo "==> drawing Command Grille as macOS shows it"
draw_icon Shelbi "$compiled" "$icon_dir/command-grille"

# Racing S is only ever drawn by the app itself (the bundle wears Command
# Grille), so its compile stays in the scratch directory.
compile_icon RacingS "$work/RacingS"
echo "==> drawing Racing S as macOS shows it"
draw_icon RacingS "$work/RacingS" "$icon_dir/racing-s"

# Retro and Retro Racing S are flat rasters: cut each down from its master.
for name in retro retro-racing-s; do
  echo "==> cutting $name from $name-1024.png"
  for size in 512 224 128; do
    sips -s format png -z "$size" "$size" "$icon_dir/$name-1024.png" --out "$icon_dir/$name-$size.png" >/dev/null
  done
done

echo "==> TUI About tables"
/usr/bin/python3 "$here/../scripts/tui-about-icon.py"

echo "done: commit desktop/assets/icon and crates/shelbi-tui/src/overlay/about_icon.rs"
