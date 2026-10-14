#!/usr/bin/env bash
# Rasterize the app icon's SVG source (assets/icon/shelbi.svg) to the 1024px
# PNG master (assets/icon/shelbi-1024.png) that bundle.sh builds Shelbi.icns
# from. Run it after editing the SVG and commit both files: the PNG is
# committed so bundling (locally and in CI) needs nothing beyond macOS's own
# sips and iconutil.
#
# Needs ImageMagick 7 (`brew install imagemagick`). Its built-in MSVG
# renderer keeps the transparent corners; Quick Look (`qlmanage -t`) paints
# them white, so it isn't a substitute.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$here/assets/icon/shelbi.svg"
out="$here/assets/icon/shelbi-1024.png"

command -v magick >/dev/null || {
  echo "render-icon.sh: needs ImageMagick 7 (brew install imagemagick)" >&2
  exit 1
}

magick -background none "MSVG:$src" -resize 1024x1024 "PNG32:$out"
echo "wrote $out"
