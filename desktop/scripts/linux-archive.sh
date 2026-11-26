#!/usr/bin/env bash
# Build the Linux release archive of the desktop app,
# shelbi-desktop_Linux_x86_64.tar.gz, plus its .sha256 (sha256sum format).
# The AUR package shelbi-desktop-bin (packaging/aur/) installs from it.
#
#   desktop/scripts/linux-archive.sh [--out DIR] [--binary PATH]
#
# --out DIR      where the archive goes (default: desktop/target/dist).
# --binary PATH  package this binary instead of running
#                `cargo build --release --locked`.
#
# The archive is flat, like the CLI's shelbi_Linux_* archives:
#   shelbi-desktop
#   dev.shelbi.Desktop.desktop
#   icons/hicolor/<n>x<n>/apps/dev.shelbi.Desktop.png   (16 to 512)
#   LICENSE
#   README.md
#
# In GitHub Actions it also writes `archive`, `sha256` and `version` to
# $GITHUB_OUTPUT.

set -euo pipefail

desktop="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$desktop/.." && pwd)"
out="$desktop/target/dist"
binary=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --binary) binary="$2"; shift 2 ;;
    -h|--help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || {
  echo "error: the Linux archive is built on x86_64 Linux" >&2
  exit 1
}

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$desktop/Cargo.toml" | head -1)"
[[ -n "$version" ]] || { echo "error: no version in desktop/Cargo.toml" >&2; exit 1; }

if [[ -z "$binary" ]]; then
  (cd "$desktop" && cargo build --release --locked)
  binary="$desktop/target/release/shelbi-desktop"
fi
[[ -x "$binary" ]] || { echo "error: no executable at $binary" >&2; exit 1; }

linux="$desktop/packaging/linux"
mkdir -p "$out"
stage="$(mktemp -d "$out/.stage.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

install -Dm755 "$binary" "$stage/shelbi-desktop"
install -Dm644 "$linux/dev.shelbi.Desktop.desktop" "$stage/dev.shelbi.Desktop.desktop"
for size in 16 24 32 48 64 128 256 512; do
  icon="icons/hicolor/${size}x${size}/apps/dev.shelbi.Desktop.png"
  install -Dm644 "$linux/$icon" "$stage/$icon"
done
install -Dm644 "$root/LICENSE" "$stage/LICENSE"
install -Dm644 "$root/README.md" "$stage/README.md"

# The binary must start and be the one this version describes. Its first line
# is the app's version; the CLI check after it (and the exit status) depend on
# the machine's `shelbi`, which a build runner needn't have.
printed="$("$stage/shelbi-desktop" --print-version 2>/dev/null | head -1 || true)"
if [[ "$printed" != "Shelbi $version" ]]; then
  echo "error: $binary reports '$printed', expected version $version" >&2
  exit 1
fi

if command -v desktop-file-validate > /dev/null; then
  desktop-file-validate "$stage/dev.shelbi.Desktop.desktop"
fi

archive="$out/shelbi-desktop_Linux_x86_64.tar.gz"
# Sorted names and a root owner, flat like the CLI archive (no ./ prefix).
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$stage" -czf "$archive" \
  shelbi-desktop dev.shelbi.Desktop.desktop icons LICENSE README.md
(cd "$out" && sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256")
sha256="$(awk '{print $1}' "$archive.sha256")"

echo "Built $archive"
echo "  version $version, sha256 $sha256"
tar -tzf "$archive" | sed 's/^/  /'

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "archive=$archive"
    echo "sha256=$sha256"
    echo "version=$version"
  } >> "$GITHUB_OUTPUT"
fi
