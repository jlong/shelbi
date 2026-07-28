#!/usr/bin/env bash
# Verify that every release archive and Debian package carries the complete
# Shelbi system plugin at the layout its installed binary resolves.

set -euo pipefail

DIST_DIR="${1:-dist}"
PLUGIN_REL="plugins/update-shelbi-configuration"
ARCHIVE_FILES=(
  "$PLUGIN_REL/.claude-plugin/plugin.json"
  "$PLUGIN_REL/.codex-plugin/plugin.json"
  "$PLUGIN_REL/skills/update-shelbi-configuration/SKILL.md"
)
DEB_PREFIX="usr/share/shelbi"

if [[ ! -d "$DIST_DIR" ]]; then
  echo "error: release directory not found: $DIST_DIR" >&2
  exit 1
fi

normalize_listing() {
  sed -e 's#^\./##' -e 's#/$##'
}

assert_listing_contains() {
  local listing="$1"
  local expected="$2"
  if ! grep -Fqx "$expected" "$listing"; then
    echo "error: package is missing $expected" >&2
    return 1
  fi
}

archive_count=0
while IFS= read -r archive; do
  archive_count=$((archive_count + 1))
  listing="$(mktemp)"
  tar -tzf "$archive" | normalize_listing > "$listing"
  for expected in "${ARCHIVE_FILES[@]}"; do
    assert_listing_contains "$listing" "$expected"
  done
  rm -f "$listing"
done < <(find "$DIST_DIR" -maxdepth 1 -type f -name 'shelbi_*.tar.gz' | sort)

if [[ "$archive_count" -eq 0 ]]; then
  echo "error: no Shelbi release archives found in $DIST_DIR" >&2
  exit 1
fi

deb_count=0
while IFS= read -r deb; do
  deb_count=$((deb_count + 1))
  data_member="$(ar t "$deb" | tr -d '\r' | sed -n '/^data\.tar\./{p;q;}')"
  if [[ -z "$data_member" ]]; then
    echo "error: Debian package has no data archive: $deb" >&2
    exit 1
  fi

  listing="$(mktemp)"
  ar p "$deb" "$data_member" | tar -tf - | normalize_listing > "$listing"
  for expected in "${ARCHIVE_FILES[@]}"; do
    assert_listing_contains "$listing" "$DEB_PREFIX/$expected"
  done
  rm -f "$listing"
done < <(find "$DIST_DIR" -maxdepth 1 -type f -name 'shelbi_*_amd64.deb' | sort)

if [[ "$deb_count" -eq 0 ]]; then
  echo "error: no Shelbi Debian package found in $DIST_DIR" >&2
  exit 1
fi

echo "verified system plugin assets in $archive_count archive(s) and $deb_count Debian package(s)"
