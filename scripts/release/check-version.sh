#!/usr/bin/env bash
set -euo pipefail

# RELEASE_TAG takes precedence: the Actions runner sets the default GITHUB_*
# variables after applying workflow env blocks, so a GITHUB_REF_NAME override
# in a workflow never reaches this script.
tag="${RELEASE_TAG:-${GITHUB_REF_NAME:-}}"

if [[ -z "$tag" ]]; then
  echo "RELEASE_TAG or GITHUB_REF_NAME is required" >&2
  exit 1
fi

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release tag must match vMAJOR.MINOR.PATCH, got: $tag" >&2
  exit 1
fi

version="$(
  cargo metadata --no-deps --format-version 1 |
    jq -r '.packages[] | select(.name == "shelbi") | .version'
)"

if [[ -z "$version" || "$version" == "null" ]]; then
  echo "could not resolve Cargo package version for shelbi" >&2
  exit 1
fi

tag_version="${tag#v}"

if [[ "$version" != "$tag_version" ]]; then
  echo "Cargo version $version != tag $tag_version" >&2
  exit 1
fi

# The desktop app is its own Cargo workspace (`desktop/`), so it can't inherit
# the workspace version; its literal `version` must track it. Resolved from the
# working directory like the root check above: release-apt.yml runs this
# script from main against the tag's tree. A tag cut before the desktop crate
# existed has no `desktop/` and has nothing to check.
desktop_manifest="desktop/Cargo.toml"
if [[ ! -f "$desktop_manifest" ]]; then
  echo "release version $version matches tag $tag (no desktop/ in this tree)"
  exit 0
fi

desktop_version="$(
  cargo metadata --no-deps --format-version 1 --manifest-path "$desktop_manifest" |
    jq -r '.packages[] | select(.name == "shelbi-desktop") | .version'
)"

if [[ -z "$desktop_version" || "$desktop_version" == "null" ]]; then
  echo "could not resolve Cargo package version for shelbi-desktop" >&2
  exit 1
fi

if [[ "$desktop_version" != "$version" ]]; then
  echo "desktop/Cargo.toml version $desktop_version != shelbi version $version" >&2
  exit 1
fi

echo "release version $version matches tag $tag (shelbi and shelbi-desktop)"
