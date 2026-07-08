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

echo "release version $version matches tag $tag"
