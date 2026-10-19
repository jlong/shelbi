#!/usr/bin/env bash
# Diff `--visual-figma` renders against John's Figma reference PNGs.
#
#   scripts/figma-diff.sh <render-dir> [ref-dir]
#
# ref-dir defaults to ~/.shelbi/projects/shelbi/design-refs/desktop-figma.
# For every reference `<screen>-<theme>.png` with a render of the same name
# it writes into `<render-dir>/diff/`:
#
#   <screen>-<theme>-side.png     the reference (left) and the render (right)
#   <screen>-<theme>-heat.png     where they differ: the per-pixel difference
#                                 (red, through orange, to yellow at the
#                                 largest) over a dimmed reference
#   <screen>-<theme>-overlay.png  the two blended 50/50
#
# and prints a table (also saved as `diff/report.txt`): the share of pixels
# that differ by more than FUZZ, and the bounding boxes (WxH+X+Y, in the
# reference's pixels) of the largest regions of differing pixels.
#
# Knobs (environment):
#   FUZZ=10      a pixel differs when a channel is off by more than FUZZ% of
#                full scale (anti-aliasing noise stays under it)
#   REGIONS=3    regions listed per screen
#   GROW=6       differing pixels this close merge into one region (a line
#                of text is one region, not one per glyph)
#   CORNER=28    the references' window corners are rounded on Figma's
#                canvas: pixels outside a rounded rect of this radius are
#                not compared
#
# A render whose size differs from its reference (a `--2x` render against
# the 1x references) is scaled to the reference's size first; to compare at
# 2x, export the frames at 2x into a directory of their own and pass it as
# ref-dir.
#
# Needs ImageMagick 7 (`brew install imagemagick`).
set -euo pipefail

usage() {
  echo "usage: figma-diff.sh <render-dir> [ref-dir]" >&2
  exit 2
}
[[ $# -ge 1 && $# -le 2 ]] || usage
render_dir="${1%/}"
ref_dir="${2:-$HOME/.shelbi/projects/shelbi/design-refs/desktop-figma}"
ref_dir="${ref_dir%/}"
fuzz="${FUZZ:-10}"
regions="${REGIONS:-3}"
grow="${GROW:-6}"
corner="${CORNER:-28}"

command -v magick >/dev/null || {
  echo "figma-diff.sh: needs ImageMagick 7 (brew install imagemagick)" >&2
  exit 1
}
[[ -d "$render_dir" ]] || { echo "figma-diff.sh: no render dir $render_dir" >&2; exit 1; }
[[ -d "$ref_dir" ]] || { echo "figma-diff.sh: no reference dir $ref_dir" >&2; exit 1; }

out="$render_dir/diff"
mkdir -p "$out"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/figma-diff.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# The side-by-side labels' font (ImageMagick has no usable default here).
font=()
for f in /System/Library/Fonts/Helvetica.ttc /System/Library/Fonts/Supplemental/Arial.ttf \
  /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf; do
  if [[ -f "$f" ]]; then
    font=(-font "$f")
    break
  fi
done

# The heatmap's color ramp: no difference is black, then red, orange, yellow.
magick -size 1x64 'gradient:black-#c00000' -size 1x64 'gradient:#c00000-#ff8c00' \
  -size 1x128 'gradient:#ff8c00-#ffff60' -append "$tmp/ramp.png"

report="$out/report.txt"
printf '%-28s %9s  %s\n' "screen" "mismatch" "largest regions (WxH+X+Y)" | tee "$report"

found=0
missing=()
for ref in "$ref_dir"/*-dark.png "$ref_dir"/*-light.png; do
  [[ -f "$ref" ]] || continue
  name="$(basename "$ref" .png)"
  render="$render_dir/$name.png"
  if [[ ! -f "$render" ]]; then
    missing+=("$name")
    continue
  fi
  found=$((found + 1))
  size="$(magick identify -format '%wx%h' "$ref")"
  w="${size%x*}"
  h="${size#*x}"
  note=""
  if [[ "$(magick identify -format '%wx%h' "$render")" != "$size" ]]; then
    magick "$render" -resize "${size}!" "$tmp/render.png"
    render="$tmp/render.png"
    note=" (render scaled to ${size})"
  fi

  # What's compared: inside the window's rounded rect.
  magick -size "$size" xc:black -fill white \
    -draw "roundrectangle 0,0 $((w - 1)),$((h - 1)) $corner,$corner" "$tmp/corner.png"
  # Per pixel, the largest channel difference (0 = identical).
  magick "$ref" "$render" -alpha off -compose difference -composite \
    -channel RGB -separate +channel -evaluate-sequence max \
    "$tmp/corner.png" -compose multiply -composite "$tmp/delta.png"
  # The pixels that differ by more than FUZZ.
  magick "$tmp/delta.png" -threshold "${fuzz}%" "$tmp/mask.png"
  pct="$(magick "$tmp/mask.png" -format '%[fx:mean*100]' info:)"

  # The largest regions: differing pixels merged across GROW px, as boxes.
  boxes="$(
    magick "$tmp/mask.png" -morphology Dilate "Square:$grow" \
      -define connected-components:verbose=true -connected-components 8 null: 2>/dev/null |
      awk '$NF ~ /^(gray\(255\)|srgb\(255,255,255\)|white)$/ { print $4, $2 }' |
      sort -rn | head -n "$regions" | awk '{ print $2 }' | paste -sd ' ' -
  )"
  printf '%-28s %8.2f%%  %s%s\n' "$name" "$pct" "${boxes:--}" "$note" | tee -a "$report"

  # Side by side, labelled.
  label() {
    magick "$1" -alpha off -background '#202020' -gravity north -splice 0x28 \
      ${font[@]+"${font[@]}"} -fill white -pointsize 15 -annotate +0+6 "$2" "$3"
  }
  label "$ref" "Figma: $name" "$tmp/a.png"
  label "$render" "Render: $name" "$tmp/b.png"
  magick "$tmp/a.png" "$tmp/b.png" -background '#202020' -splice 8x0 +append -chop 8x0 \
    "$out/$name-side.png"
  # The difference, colored, over the reference dimmed to gray.
  magick \( "$ref" -alpha off -colorspace gray -evaluate multiply 0.3 -colorspace sRGB \) \
    \( "$tmp/delta.png" -level "0,50%" "$tmp/ramp.png" -clut \) \
    -compose screen -composite "$out/$name-heat.png"
  # Half and half.
  magick "$ref" "$render" -alpha off -compose blend -define compose:args=50 -composite \
    "$out/$name-overlay.png"
done

if [[ ${#missing[@]} -gt 0 ]]; then
  echo "no render for: ${missing[*]}" | tee -a "$report"
fi
echo "$found screen(s) compared; images in $out" | tee -a "$report"
[[ $found -gt 0 ]]
