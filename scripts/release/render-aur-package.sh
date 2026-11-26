#!/usr/bin/env bash
# Render one of Shelbi's AUR packages (packaging/aur/<package>/PKGBUILD.in)
# for a release: fill in the version and the archive's SHA-256, then write
# PKGBUILD and .SRCINFO into an output directory, ready for `makepkg -si` or a
# push to the package's AUR repository.
#
#   render-aur-package.sh --package shelbi-bin --version 0.14.0 --out DIR
#   render-aur-package.sh --package shelbi-desktop-bin --version 0.14.0 \
#     --archive dist/shelbi-desktop_Linux_x86_64.tar.gz --out DIR
#
# Where the SHA-256 comes from, first match wins:
#   --sha256 HEX    as given.
#   --archive FILE  hashed from a local archive, which is also copied into DIR
#                   under the source's file name, so makepkg uses it instead of
#                   downloading (a local build before the release exists).
#   (neither)       the published release: checksums.txt for the CLI archive,
#                   shelbi-desktop_Linux_x86_64.tar.gz.sha256 for the app's.
#
# .SRCINFO needs makepkg (Arch), run as a non-root user. --no-srcinfo skips it.

set -euo pipefail

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

package=""
version=""
out=""
sha256=""
archive=""
srcinfo=true
repo="${GITHUB_REPOSITORY:-jlong/shelbi}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --package) package="$2"; shift 2 ;;
    --version) version="${2#v}"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --sha256) sha256="$2"; shift 2 ;;
    --archive) archive="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --no-srcinfo) srcinfo=false; shift ;;
    -h|--help) usage 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done

[[ -n "$package" && -n "$version" && -n "$out" ]] || usage

root="$(cd "$(dirname "$0")/../.." && pwd)"
template="$root/packaging/aur/$package/PKGBUILD.in"
[[ -f "$template" ]] || { echo "error: no template at $template" >&2; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: --version must be X.Y.Z (pacman's pkgver has no '-'), got $version" >&2
  exit 1
}

case "$package" in
  shelbi-bin)
    asset="shelbi_Linux_x86_64.tar.gz"
    local_name="shelbi-${version}-x86_64.tar.gz"
    ;;
  shelbi-desktop-bin)
    asset="shelbi-desktop_Linux_x86_64.tar.gz"
    local_name="shelbi-desktop-${version}-x86_64.tar.gz"
    ;;
  *) echo "error: unknown package $package" >&2; exit 1 ;;
esac

mkdir -p "$out"

if [[ -z "$sha256" && -n "$archive" ]]; then
  sha256="$(sha256sum "$archive" | awk '{print $1}')"
  cp "$archive" "$out/$local_name"
elif [[ -z "$sha256" ]]; then
  base="https://github.com/$repo/releases/download/v$version"
  if [[ "$package" == shelbi-bin ]]; then
    sha256="$(curl -fsSL "$base/checksums.txt" | awk -v f="$asset" '$2 == f {print $1}')"
  else
    sha256="$(curl -fsSL "$base/$asset.sha256" | awk '{print $1}')"
  fi
fi

[[ "$sha256" =~ ^[0-9a-f]{64}$ ]] || {
  echo "error: no SHA-256 for $asset (got '${sha256}')" >&2
  exit 1
}

sed -e "s/@PKGVER@/$version/g" -e "s/@SHA256@/$sha256/g" "$template" > "$out/PKGBUILD"
if grep -n '@[A-Z0-9]*@' "$out/PKGBUILD"; then
  echo "error: unfilled placeholder left in $out/PKGBUILD" >&2
  exit 1
fi

if $srcinfo; then
  (cd "$out" && makepkg --printsrcinfo > .SRCINFO)
fi

echo "Rendered $package $version into $out (sha256 $sha256)"
