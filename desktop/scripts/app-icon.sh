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
# Retro, from its 1024px master (assets/icon/retro-1024.png):
#
#   assets/icon/retro-{512,224,128}.png
#
# and both TUI About tables (crates/shelbi-tui/src/overlay/about_icon.rs).
#
# bundle.sh only copies the compiled files, so a release never needs Xcode
# 26; commit what this script writes.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # desktop/
icon_dir="$here/assets/icon"
source="$icon_dir/Shelbi.icon"
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
app="$work/IconRender.app"
cleanup() {
  if [[ -d "$app" ]]; then "$lsregister" -u "$app" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

# The digest bundle.sh checks: every file in the .icon, by path and content.
source_digest() {
  (cd "$icon_dir" && find Shelbi.icon -type f ! -name .DS_Store -print0 | LC_ALL=C sort -z |
    xargs -0 shasum -a 256) | shasum -a 256 | cut -c1-64
}

echo "==> compiling $source (actool $actool_version)"
xcrun actool "$source" \
  --compile "$work" \
  --app-icon Shelbi \
  --output-partial-info-plist "$work/partial.plist" \
  --platform macosx \
  --minimum-deployment-target 15.0 \
  --include-all-app-icons >/dev/null
[[ -s "$work/Assets.car" && -s "$work/Shelbi.icns" ]] || fail "actool wrote no Assets.car / Shelbi.icns"
mkdir -p "$compiled"
cp "$work/Assets.car" "$work/Shelbi.icns" "$compiled/"
source_digest >"$compiled/Shelbi.icon.sha256"

echo "==> drawing Command Grille as macOS shows it"
# A throwaway app around the compiled icon (any executable will do; without
# one Finder badges the icon), registered so NSWorkspace draws its icon.
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$compiled/Assets.car" "$compiled/Shelbi.icns" "$app/Contents/Resources/"
cp /usr/bin/true "$app/Contents/MacOS/IconRender"
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>IconRender</string>
	<key>CFBundleIdentifier</key>
	<string>dev.shelbi.icon-render.$$</string>
	<key>CFBundleIconFile</key>
	<string>Shelbi</string>
	<key>CFBundleIconName</key>
	<string>Shelbi</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1
"$lsregister" -f "$app"
xcrun swiftc -O "$here/scripts/render-app-icon.swift" -o "$work/render-app-icon"
for size in 512 224 128; do
  "$work/render-app-icon" "$app" "$size" "$icon_dir/command-grille-$size.png"
done

echo "==> cutting Retro from retro-1024.png"
for size in 512 224 128; do
  sips -s format png -z "$size" "$size" "$icon_dir/retro-1024.png" --out "$icon_dir/retro-$size.png" >/dev/null
done

echo "==> TUI About tables"
/usr/bin/python3 "$here/../scripts/tui-about-icon.py"

echo "done: commit desktop/assets/icon and crates/shelbi-tui/src/overlay/about_icon.rs"
